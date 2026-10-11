# Fuzzing

Every parser of on-disk data has a target here. Inputs are hostile by definition.

```sh
cargo +nightly-2026-10-01 fuzz run part_tables -- -max_total_time=60
```

CI runs each target for a short time on every pull request and longer on a schedule.
