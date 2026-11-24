//! Times the calls the SwiftUI shell makes, without the UI:
//! `cargo run --release -p sapu-ffi --example scan_bench -- <folder> [runs]`
//! Prints one JSON line per run. Sapu AI is off so runs don't change your model.

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: scan_bench <folder> [runs]");
    let runs: usize = args.next().and_then(|r| r.parse().ok()).unwrap_or(3);
    for run in 1..=runs {
        let scanner = sapu_ffi::DiskScanner::new();
        let t = Instant::now();
        let result = scanner.scan(path.clone(), false).expect("scan failed");
        let scan_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let top = result.children(Vec::new()).expect("children");
        let children_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let suggestions = result.suggestions();
        let suggestions_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let largest = result.largest_files(100);
        let largest_ms = t.elapsed().as_secs_f64() * 1000.0;

        let s = result.summary();
        println!(
            "{{\"run\":{run},\"files\":{},\"dirs\":{},\"scan_ms\":{scan_ms:.1},\"children_ms\":{children_ms:.2},\
             \"top_level\":{},\"suggestions_ms\":{suggestions_ms:.2},\"suggestions\":{},\"largest_ms\":{largest_ms:.2},\
             \"largest\":{}}}",
            s.files,
            s.dirs,
            top.len(),
            suggestions.len(),
            largest.len()
        );
    }
}
