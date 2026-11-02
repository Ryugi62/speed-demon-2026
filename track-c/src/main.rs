//! Container entrypoint: /data/records.csv + /data/regions.csv -> /out/aggregates.txt
use std::{env, fs, process};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let data = arg(&args, "--data").unwrap_or_else(|| "/data".into());
    let out = arg(&args, "--out").unwrap_or_else(|| "/out".into());
    let mut opts = sdjoin::Opts::default();
    opts.threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    if let Some(t) = arg(&args, "--threads") {
        opts.threads = t.parse().map_err(|_| "bad --threads")?;
    }
    if let Some(w) = arg(&args, "--window") {
        opts.window = w.parse().map_err(|_| "bad --window")?;
    }
    let regions = fs::read(format!("{data}/regions.csv")).map_err(|e| format!("regions.csv: {e}"))?;
    let records = fs::File::open(format!("{data}/records.csv")).map_err(|e| format!("records.csv: {e}"))?;
    let (text, info) = sdjoin::aggregate(&records, &regions, &opts)?;
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    fs::write(format!("{out}/aggregates.txt"), text).map_err(|e| format!("aggregates.txt: {e}"))?;
    if env::var_os("SD_REPORT").is_some() {
        let hwm = fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with("VmHWM")).map(str::to_string));
        eprintln!("segments={} redone={} {}", info.segments, info.redone, hwm.unwrap_or_default());
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
