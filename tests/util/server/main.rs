// Copyright 2018-2026 the Deno authors. MIT license.

#![allow(clippy::print_stdout, reason = "test server output")]
#![allow(clippy::print_stderr, reason = "test server output")]

pub use test_util::*;

pub mod https;
pub mod npm;
pub mod servers;

fn main() {
  rustls::crypto::aws_lc_rs::default_provider()
    .install_default()
    .unwrap();
  setup_panic_hook();
  if std::env::var_os("TEST_SERVER_EXIT_ON_STDIN_EOF").is_some() {
    std::thread::spawn(|| {
      let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
      std::process::exit(0);
    });
  }
  servers::run_all_servers();
}

fn setup_panic_hook() {
  // Tokio does not exit the process when a task panics, so we define a custom
  // panic hook to implement this behaviour.
  let orig_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |panic_info| {
    std::eprintln!(
      "\n============================================================"
    );
    std::eprintln!("Test server panicked!\n");
    orig_hook(panic_info);
    std::process::exit(1);
  }));
}
