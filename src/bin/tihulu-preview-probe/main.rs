// SPDX-License-Identifier: AGPL-3.0-only

mod metrics;
mod protocol;

use std::{env, path::PathBuf, process::ExitCode};

use metrics::{GrowthWatch, SampleWriter, find_cosmic_comp, process_metrics};
use protocol::ProbeWayland;

const DEFAULT_CAPTURES: usize = 500;
const DEFAULT_SAMPLE_EVERY: usize = 10;

#[derive(Debug)]
struct Args {
    list: bool,
    match_term: Option<String>,
    captures: usize,
    sample_every: usize,
    output: PathBuf,
    circuit_breaker: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            list: false,
            match_term: None,
            captures: DEFAULT_CAPTURES,
            sample_every: DEFAULT_SAMPLE_EVERY,
            output: PathBuf::from("tihulu-preview-probe.csv"),
            circuit_breaker: true,
        }
    }
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Self::default();
        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--list" => args.list = true,
                "--match" => {
                    args.match_term = Some(
                        iter.next()
                            .ok_or_else(|| "--match requires a value".to_owned())?,
                    );
                }
                "--captures" => args.captures = parse_positive("--captures", iter.next())?,
                "--sample-every" => {
                    args.sample_every = parse_positive("--sample-every", iter.next())?;
                }
                "--output" => {
                    args.output = PathBuf::from(
                        iter.next()
                            .ok_or_else(|| "--output requires a path".to_owned())?,
                    );
                }
                "--no-circuit-breaker" => args.circuit_breaker = false,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(args)
    }
}

fn parse_positive(flag: &str, value: Option<String>) -> Result<usize, String> {
    let raw = value.ok_or_else(|| format!("{flag} requires a value"))?;
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| format!("{flag} expects a positive integer, got {raw:?}"))?;
    if parsed == 0 {
        return Err(format!("{flag} must be greater than zero"));
    }
    Ok(parsed)
}

fn print_help() {
    println!(
        "tihulu-preview-probe\n\
Stress-test COSMIC foreign-toplevel ext-image-copy-capture resource lifetime.\n\n\
Usage:\n\
  tihulu-preview-probe --list\n\
  tihulu-preview-probe --match brave [--captures 500] [--sample-every 10] [--output FILE]\n\n\
Options:\n\
  --list                  List available ext-foreign-toplevel handles and exit\n\
  --match TEXT            Select first title/app-id/identifier containing TEXT\n\
  --captures N            Sequential captures (default: 500)\n\
  --sample-every N        /proc sampling interval (default: 10 captures)\n\
  --output FILE           CSV output (default: tihulu-preview-probe.csv)\n\
  --no-circuit-breaker    Record even after monotonic FD growth (not recommended)\n\n\
The standard ext-foreign-toplevel-list protocol does not expose minimized state.\n\
Minimize the target window before starting the capture run."
    );
}

fn run() -> Result<ExitCode, String> {
    let args = Args::parse()?;
    let mut wayland = ProbeWayland::connect()?;

    if args.list {
        wayland.list_toplevels();
        return Ok(ExitCode::SUCCESS);
    }

    wayland.verify_capture_globals()?;
    let source = wayland.source_for(args.match_term.as_deref())?;
    let comp_pid = find_cosmic_comp();

    eprintln!(
        "probe target selected; captures={} sample_every={} cosmic_comp_pid={:?}",
        args.captures, args.sample_every, comp_pid
    );
    eprintln!(
        "IMPORTANT: ext-foreign-toplevel-list cannot prove minimized state; keep the selected target minimized for this run."
    );

    let mut samples = SampleWriter::create(&args.output)
        .map_err(|error| format!("could not create {}: {error}", args.output.display()))?;
    let mut growth = GrowthWatch::default();

    let baseline_probe = process_metrics(std::process::id());
    let baseline_comp = comp_pid.map(process_metrics);
    samples
        .record("baseline", 0, None, baseline_probe, comp_pid, baseline_comp)
        .map_err(|error| format!("could not write baseline sample: {error}"))?;
    let _ = growth.push(baseline_probe, baseline_comp);

    let mut consecutive_failures = 0usize;
    let mut breaker_reason = None;
    for iteration in 1..=args.captures {
        let capture = wayland.capture_once(&source);
        let ok = capture.is_ok();
        if ok {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            if let Err(error) = &capture {
                eprintln!("capture {iteration} failed: {error}");
            }
        }

        let should_sample = iteration % args.sample_every == 0 || iteration == args.captures || !ok;
        if should_sample {
            let probe = process_metrics(std::process::id());
            let comp = comp_pid.map(process_metrics);
            samples
                .record("capture", iteration, Some(ok), probe, comp_pid, comp)
                .map_err(|error| format!("could not write CSV sample: {error}"))?;
            eprintln!(
                "sample iteration={iteration} probe_fd={} comp_fd={} comp_capture_memfd={}",
                probe.fd_count,
                comp.map(|metrics| metrics.fd_count).unwrap_or(0),
                comp.map(|metrics| metrics.capture_memfd_count).unwrap_or(0)
            );

            if args.circuit_breaker
                && let Some(reason) = growth.push(probe, comp)
            {
                breaker_reason = Some(reason);
                break;
            }
        }

        if consecutive_failures >= 3 {
            breaker_reason = Some("three consecutive capture failures");
            break;
        }
    }

    wayland.destroy_source(source);

    if let Some(reason) = breaker_reason {
        eprintln!("CIRCUIT BREAKER: {reason}");
        eprintln!("Preview capture is NOT approved for integration from this run.");
        return Ok(ExitCode::from(2));
    }

    eprintln!("probe completed; results: {}", args.output.display());
    eprintln!(
        "Completion alone is not approval: inspect FD/memfd/RSS trends before integrating previewd."
    );
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tihulu-preview-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
