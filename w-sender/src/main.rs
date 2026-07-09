mod args;
mod metrics;
mod packetizer;
mod wasapi;

use anyhow::{anyhow, Result};
use args::Args;
use clap::Parser;
use metrics::{Metrics, MetricsPrinter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let args = Args::parse();
    args.validate()?;

    if args.list_devices {
        return wasapi::list_capture_devices();
    }

    let metrics = Arc::new(Metrics::new());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_args = args.clone();
    let worker_metrics = Arc::clone(&metrics);
    let worker_stop = Arc::clone(&stop);
    let worker =
        thread::spawn(move || wasapi::run_capture_sender(worker_args, worker_metrics, worker_stop));

    let mut printer = MetricsPrinter::new(args.metrics_interval_sec, Arc::clone(&metrics));
    let start = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(50));
        printer.maybe_print();

        if args
            .duration_sec
            .map(|duration| start.elapsed() >= Duration::from_secs_f64(duration))
            .unwrap_or(false)
        {
            stop.store(true, Ordering::Relaxed);
            break;
        }

        if worker.is_finished() {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    worker
        .join()
        .map_err(|_| anyhow!("w-sender capture thread panicked"))?
}
