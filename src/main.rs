/*
* Tests the example from https://learn.microsoft.com/en-us/windows/win32/ipc/multithreaded-pipe-server
*/

use anyhow::Context;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
    },
    ptr,
    thread::{self, JoinHandle},
    time::Duration,
};
use winapi::{
    shared::{
        minwindef::DWORD,
        ntdef::HANDLE,
        winerror::{ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED},
    },
    um::{
        errhandlingapi::GetLastError,
        fileapi::{CreateFileW, OPEN_EXISTING},
        handleapi::{CloseHandle, DuplicateHandle, INVALID_HANDLE_VALUE},
        namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW},
        processthreadsapi::GetCurrentProcess,
        winbase::{
            PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE, PIPE_TYPE_BYTE, PIPE_TYPE_MESSAGE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
        winnt::{FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE},
    },
};

macro_rules! call_BOOL_with_last_error {
    ($call: expr) => {
        if ($call) != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
}
macro_rules! call_with_last_error {
    ($call: expr) => {{
        let value = $call;
        if value != INVALID_HANDLE_VALUE {
            Ok(value)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }};
}
const BUFSIZE: DWORD = 512;

fn main() -> anyhow::Result<()> {
    let pipe_name = OsString::from("\\\\.\\pipe\\testpipe");
    let mut pipe_name = pipe_name.as_os_str().encode_wide().collect::<Vec<_>>();
    pipe_name.push(0);

    let server_pipe_name = pipe_name.clone();

    let server_thread: JoinHandle<Result<(), std::io::Error>> = std::thread::Builder::new()
        .name("pipe_server_listener".into())
        .spawn(move || {
            loop {
                let server_listener_pipe_handle = unsafe {
                    CreateNamedPipeW(
                        server_pipe_name.as_ptr(), // pipe name
                        PIPE_ACCESS_DUPLEX,        // read/write access
                        PIPE_TYPE_BYTE |       // message type pipe 
                        PIPE_WAIT, // blocking mode
                        PIPE_UNLIMITED_INSTANCES,  // max. instances
                        BUFSIZE,                   // output buffer size
                        BUFSIZE,                   // input buffer size
                        0,                         // client time-out
                        ptr::null_mut(),
                    ) // default security attribute
                };

                if server_listener_pipe_handle == INVALID_HANDLE_VALUE {
                    return Err(std::io::Error::last_os_error());
                }

                // Wait for the client to connect; if it succeeds,
                // the function returns a nonzero value. If the function
                // returns zero, GetLastError returns ERROR_PIPE_CONNECTED.

                let mut hPipe: HANDLE = server_listener_pipe_handle;
                let connected = if unsafe { ConnectNamedPipe(hPipe, ptr::null_mut()) } != 0 {
                    Ok(())
                } else {
                    let os_error_code = unsafe { GetLastError() };
                    if ERROR_PIPE_CONNECTED == os_error_code {
                        Ok(())
                    } else {
                        let os_error = io::Error::from_raw_os_error(os_error_code as i32);

                        Err(os_error)
                    }
                };

                if connected.is_ok() {
                    println!("Client connected, creating a processing thread.");

                    // Create a thread for this client.
                    let hPipe = PipeHandle::from(hPipe);
                    let receiver_handle = hPipe.try_clone().expect("Failed to clone handle");
                    let server_receiver_thread =
                        spawn_receiver(receiver_handle, "server_receiver_thread", true)
                            .expect("Could not spawn receiver thread");
                } else {
                    println!("Shutting down server: {:?}", connected);
                    // The client could not connect, so close the pipe.
                    unsafe { CloseHandle(hPipe) };
                }
            }
        })
        .expect("Failed to launch listener");

    thread::sleep(Duration::from_secs(1));

    let client = loop {
        let client: Result<HANDLE, io::Error> = call_with_last_error!(unsafe {
            CreateFileW(
                pipe_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        });

        match client {
            Ok(handle) => break handle,
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                continue;
            }
            Err(err) => {
                return Err::<(), anyhow::Error>(err.into()).context("Creating Client file")
            }
        };
    };
    let client = PipeHandle::from(client);

    let client_receiver = spawn_receiver(
        client.try_clone().expect("Failed to clone handle"),
        "client receiver",
        false,
    )
    .expect("Failed to launch Receiver on client side");

    let mut sender = File::from(client);
    let message = b"Hello";
    sender
        .write_all(message)
        .context("Failed to write to pipe")?;
    sender.flush()?;

    thread::sleep(Duration::from_secs(10));

    Ok(())
}

fn spawn_receiver(
    receiver_handle: PipeHandle,
    thread_name: &'static str,
    should_echo: bool,
) -> Result<thread::JoinHandle<()>, io::Error> {
    thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let mut reader =
                std::fs::File::from(receiver_handle.try_clone().expect("Failed to clone handle"));
            let mut buf = [0u8; BUFSIZE as usize];
            loop {
                match reader.read(&mut buf[..]) {
                    Ok(consumed) => {
                        println!("{} received {} bytes", thread_name, consumed);
                        if should_echo {
                            let mut answer: Vec<u8> = Vec::new();
                            answer.extend_from_slice(&buf[..consumed]);
                            let writer_handle =
                                receiver_handle.try_clone().expect("Failed to clone handle");
                            thread::spawn(move || {
                                thread::sleep(Duration::from_secs_f32(0.1));
                                let mut writer = File::from(writer_handle);
                                println!("{} answering {} bytes", thread_name, answer.len());
                                writer
                                    .write_all(answer.as_slice())
                                    .expect("Failed to reply");
                                println!("{} answered {} bytes", thread_name, answer.len())
                            });
                        }
                    }
                    Err(err) => {
                        println!("{} detected Error {}", thread_name, err);
                        break;
                    }
                }
            }
        })
}

struct PipeHandle(OwnedHandle);

impl PipeHandle {
    pub fn try_clone(&self) -> Result<Self, std::io::Error> {
        let mut dup_handle: HANDLE = ptr::null_mut();
        call_BOOL_with_last_error!(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.0.as_raw_handle() as _,
                GetCurrentProcess(),
                (&mut dup_handle) as _,
                GENERIC_READ | GENERIC_WRITE,
                0,
                0,
            )
        })
        .map(|_| Self::from(dup_handle))
    }
}

impl IntoRawHandle for PipeHandle {
    fn into_raw_handle(self) -> std::os::windows::prelude::RawHandle {
        self.0.into_raw_handle()
    }
}

impl From<HANDLE> for PipeHandle {
    fn from(value: HANDLE) -> Self {
        let handle = unsafe { OwnedHandle::from_raw_handle(value as _) };
        Self(handle)
    }
}

impl From<PipeHandle> for std::fs::File {
    fn from(value: PipeHandle) -> Self {
        value.0.into()
    }
}

unsafe impl Send for PipeHandle {}
