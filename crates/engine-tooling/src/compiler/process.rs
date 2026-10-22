//! Child process ownership and bounded compiler output capture.

use super::{BuildFailure, failure, io_error};
use process_wrap::std::{ChildWrapper, CommandWrap};
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub(super) struct CompilerOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) cancelled: bool,
}

pub(super) fn run_compiler(
    mut command: Command,
    cancel: &AtomicBool,
) -> Result<CompilerOutput, BuildFailure> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = spawn_owned(command)?;
    let stdout = child.stdout().take().expect("piped stdout");
    let stderr = child.stderr().take().expect("piped stderr");
    let (send, receive) = mpsc::channel();
    let stdout_send = send.clone();
    let out_thread = thread::spawn(move || {
        let _ = stdout_send.send((true, read_bounded(stdout)));
    });
    let err_thread = thread::spawn(move || {
        let _ = send.send((false, read_bounded(stderr)));
    });
    let mut was_cancelled = false;
    let mut status = None;
    let mut drain_deadline = None;
    let mut leaked_output = false;
    let mut received = 0;
    let mut stdout = String::new();
    let mut stderr = String::new();
    while status.is_none() || received < 2 {
        if cancel.load(Ordering::Acquire) && !was_cancelled {
            terminate_tree(&mut child)?;
            was_cancelled = true;
            if status.is_none() {
                status = Some(reap(&mut child)?);
            }
        }
        if status.is_none()
            && let Some(exit) = child.try_wait().map_err(io_error)?
        {
            status = Some(exit);
            drain_deadline = Some(std::time::Instant::now() + Duration::from_secs(2));
        }
        match receive.recv_timeout(Duration::from_millis(20)) {
            Ok((is_stdout, output)) => {
                let output = output.map_err(io_error)?;
                if is_stdout {
                    stdout = output;
                } else {
                    stderr = output;
                }
                received += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) if received < 2 => {
                return Err(failure(
                    "compiler.output",
                    "Compiler output reader disconnected",
                ));
            }
            _ => {}
        }
        if received < 2
            && !leaked_output
            && drain_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            terminate_tree(&mut child)?;
            leaked_output = true;
        }
    }
    // Cargo may have exited while an authored build-script descendant remains.
    // End the entire owned job/group even when the leader already exited.
    terminate_tree(&mut child)?;
    out_thread
        .join()
        .map_err(|_| failure("compiler.output", "stdout reader panicked"))?;
    err_thread
        .join()
        .map_err(|_| failure("compiler.output", "stderr reader panicked"))?;
    if leaked_output && !was_cancelled {
        return Err(failure(
            "compiler.output_leak",
            "Compiler descendant retained output pipes after Cargo exited",
        ));
    }
    Ok(CompilerOutput {
        status: status.expect("compiler leader was reaped"),
        stdout,
        stderr,
        cancelled: was_cancelled,
    })
}

pub(super) fn read_bounded(mut reader: impl Read) -> std::io::Result<String> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if result.len() < OUTPUT_LIMIT {
            result.extend_from_slice(&buffer[..count.min(OUTPUT_LIMIT - result.len())]);
        }
    }
    Ok(String::from_utf8_lossy(&result).into_owned())
}

pub(super) fn spawn_owned(command: Command) -> Result<Box<dyn ChildWrapper>, BuildFailure> {
    let mut command = CommandWrap::from(command);
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    command.spawn().map_err(io_error)
}

/// Waits for the leader after termination by polling. process_wrap's Windows
/// `JobObject::wait` blocks on the job completion port without a timeout, while
/// `try_wait` drains that port without caching the status, so calling `wait`
/// after any `try_wait` can block forever once the job's messages are consumed.
pub(super) fn reap(child: &mut Box<dyn ChildWrapper>) -> Result<ExitStatus, BuildFailure> {
    loop {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn terminate_tree(child: &mut Box<dyn ChildWrapper>) -> Result<(), BuildFailure> {
    if let Err(error) = child.start_kill() {
        #[cfg(unix)]
        if error.raw_os_error() == Some(3) && child.try_wait().map_err(io_error)?.is_some() {
            return Ok(());
        }
        return Err(io_error(error));
    }
    Ok(())
}
