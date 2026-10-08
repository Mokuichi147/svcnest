use anyhow::{Context, Result};
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use tokio::process::Child;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::*,
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

pub struct Job {
    handle: HANDLE,
    pid: u32,
}
// ハンドルの所有者は Job だけであり、Win32 の Job API はスレッド間で使用可能。
unsafe impl Send for Job {}

impl Job {
    pub fn assign_and_resume(child: &Child, pid: u32) -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let job = Self { handle, pid };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let process = child.raw_handle().context("Child has no process handle")?;
        if unsafe { AssignProcessToJobObject(handle, process.cast()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        resume_main_thread(pid)?;
        Ok(job)
    }
    pub fn graceful(&mut self, _interrupted: bool) -> Result<()> {
        // GUI 等が console signal に対応していない場合も、期限後には Job 全体を停止する。
        unsafe {
            GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid);
        }
        Ok(())
    }
    pub fn force(&mut self) -> Result<()> {
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn alive(&mut self) -> Result<bool> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(info.ActiveProcesses > 0)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn resume_main_thread(pid: u32) -> Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut entry: THREADENTRY32 = unsafe { zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let mut result = Err(anyhow::anyhow!("Cannot find the suspended target thread"));
    while found {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                result = Err(std::io::Error::last_os_error().into());
            } else {
                let resumed = unsafe { ResumeThread(thread) };
                unsafe {
                    CloseHandle(thread);
                }
                result = if resumed == u32::MAX {
                    Err(std::io::Error::last_os_error().into())
                } else {
                    Ok(())
                };
            }
            break;
        }
        found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

pub fn prepare_console() {
    // runner の CREATE_NO_WINDOW でも console signal は使える。
    // GetConsoleWindow が null でも、表示用 console を追加で割り当てない。
    // foreground の対象へ Ctrl+C の無視設定を継承させない。
    unsafe {
        SetConsoleCtrlHandler(None, 0);
    }
}
