# Probe command quick reference

```bash
cargo build --release --bin tihulu-preview-probe
./target/release/tihulu-preview-probe --list
./target/release/tihulu-preview-probe --match brave --captures 500 --sample-every 10 --output brave-500.csv
```

Keep the selected target minimized while the capture run executes. Do not integrate preview code based only on a successful exit; inspect the CSV trends and the circuit-breaker result.
