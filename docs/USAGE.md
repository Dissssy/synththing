# Usage data

Anonymous usage data tells the developers which warnings, errors and crashes people run into, including the ones nobody reports. It's opt in: the app asks once, on the first launch that has it, and the switch is in Preferences > App > Usage data (each device decides for itself; it isn't synced). Branch builds never send it.

## What the app sends

The app (`src/usage.rs`) counts every log line it keeps (its own from info up, other crates' from warn up) and every panic. Once a day it sends the counts to the official server; after a panic it sends them as soon as it starts again, without waiting for the day. The counts wait in `<config dir>/usage.json` until they're sent, and turning usage data off deletes them.

A report (`library::usage::UsageReport`) holds:

- the release (`1.8.3`, or `1.8.3 nightly` for a canary build) and the platform (`windows x86_64`);
- how many times the app was started;
- for each kind of line, its level (`panic`, `error`, `warn`, `info`), target (the log target, or the thread for a panic), signature and count.

A log line's signature is where it was logged, module and line (`synththing::library::sync:212`), and its text only when that text is fixed in the code. What a message was filled in with (song names, paths, error details) never leaves the device, so the line has to be read in the code at that version. A panic's signature is its source file and line, from the crate's folder on, and its message with numbers turned into `#`, quoted text into `"…"`, and anything with a slash into `<path>`.

Nothing in a report says which device or person sent it: no key, no install ID. Preferences > App > Usage data > See what's sent shows the next report so far.

## Rules

The server decides which lines it wants, with rules of the form `<level> <target> keep|drop`. The level is one of the four or `*`; the target is a log target, a prefix ending in `*` (`synththing::library::*`), or `*`. The first rule a line matches decides it, a line no rule matches is dropped, and panics are always kept. The defaults keep errors and warnings and drop info:

```
error * keep
warn * keep
info * drop
```

The app fetches the rules (`GET /api/v1/usage/rules`) before each report and sends only what they keep; the server applies them again to what arrives. Change them with `synththing admin usage-rules --set "<rule>" --set "<rule>"...` (`--set ""` goes back to the defaults); `synththing admin usage-rules` shows them.

## On the server

`server/usage.rs` adds each report into one row per version, platform, level, target and signature, with its total, the count that's `new` since intake last took it, and when it was first and last seen; app starts are counted per version and platform. No address is stored with any of it (reports are limited to 6 a day per address, counted in memory).

`synththing admin usage [path...]` shows the tree one level at a time, biggest first: versions, then under a version its platforms, then levels, targets and signatures, e.g. `synththing admin usage 1.8.3 "windows x86_64" warn`. Versions and platforms show their app starts, so counts can be read as rates. Nothing is deleted until `synththing admin usage [path...] --clear` deletes that part of the tree (all of it without a path).
