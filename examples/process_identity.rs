//! Print identity metadata only; never print arguments or environment values.
fn main() {
    for pid in std::env::args()
        .skip(1)
        .filter_map(|s| s.parse::<u32>().ok())
    {
        match ghostback::native::process(pid) {
            Ok(p) => println!(
                "pid={} name={:?} executable={} argv0={:?} birth={} parent={}",
                p.pid,
                p.name,
                p.executable.display(),
                p.argv.first(),
                p.started_us,
                p.parent
            ),
            Err(e) => eprintln!("{pid}: {e}"),
        }
    }
}
