//! Start the real media origin over a directory and print the URL for one file.
//!
//! `scripts/video/verify-linux-playback.py` drives a WebKitGTK view against this URL, so the
//! playback check exercises the shipping server rather than a stand-in.
//!
//! It is an example rather than a binary on purpose: an extra `src/bin` entry makes the Tauri
//! bundler pick the wrong executable as the application.
//!
//! Usage: `cargo run --example media-origin-probe -- <root-directory> <file-inside-root>`

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use soundar_desktop_lib::video::LocalMediaServer;

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let (Some(root), Some(file)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: media-origin-probe <root-directory> <file-inside-root>");
        return ExitCode::FAILURE;
    };
    // Mirror the shell: ensure the root exists before starting, so a first run still gets an origin.
    let root = PathBuf::from(root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!("could not ensure the root exists: {error}");
        return ExitCode::FAILURE;
    }
    let server = match LocalMediaServer::start(vec![root]) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("media origin failed to start: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", server.url_for(&PathBuf::from(file)));
    if io::stdout().flush().is_err() {
        return ExitCode::FAILURE;
    }
    // Stay alive until the caller closes stdin, then drop the server and stop the accept loop.
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    ExitCode::SUCCESS
}
