// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let args = sqleq_check::cli::Args::parse();
    sqleq_check::proc::install_interrupt_handler();
    ExitCode::from(sqleq_check::app::main(args) as u8)
}
