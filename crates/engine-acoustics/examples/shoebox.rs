//! Traces the section 8 reference shoebox on the CPU and prints the fitted RT60
//! against Eyring. With a path argument it also writes the histogram as CSV
//! (the format stream C compares against).
//!
//! `cargo run -p engine-acoustics --release --example shoebox -- [histogram.csv] [rays]`

use engine_acoustics::reference::*;
use engine_acoustics::{BANDS, fit_bands};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let csv = args.next();
    let rays = args.next().map_or(Ok(65_536), |r| r.parse::<u32>())?;
    let world = reference_shoebox(1.0)?;
    let start = std::time::Instant::now();
    let h = cpu_reference_histogram(
        &world,
        &reference_trace_settings(3.0),
        &reference_job(rays, 7),
        8,
    )?;
    let elapsed = start.elapsed();
    let fits = fit_bands(&h, 0.08)?;
    println!(
        "{rays} rays in {elapsed:?}; mean free path {:.4} m (4V/S = 5.1064 m)",
        h.mean_free_path_m
    );
    for b in 0..BANDS {
        let eyring = SHOEBOX_EYRING_RT60_S[b];
        println!(
            "band {b}: T30 {:.3} s ({:?}), Eyring {eyring:.3} s, {:+.1}%",
            fits[b].rt60_s,
            fits[b].method,
            (fits[b].rt60_s / eyring - 1.0) * 100.0
        );
    }
    if let Some(path) = csv {
        std::fs::write(&path, histogram_csv(&h))?;
        println!("wrote {path}");
    }
    Ok(())
}
