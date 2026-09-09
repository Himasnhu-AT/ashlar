# Privacy and collected fields

This page lists everything the SDK can send. If a field is not here, it is not collected. Change this page in the same commit as any change to what is collected.

## Principles

- **The host app chooses.** It declares its events at build time and picks which to listen to at startup. Undeclared events cannot be recorded.
- **Nothing is sent before the notice.** Telemetry is on by default with a first-run notice. Until the host app reports that the user has seen that notice, data is recorded on the device but nothing leaves it. A category the user switches off in the notice is deleted and never sent. (A host app that shows its notice elsewhere, such as in an installer, can lift this.)
- **The user can switch it off.** Each category — crash reports, performance, usage — has its own switch. `DO_NOT_TRACK=1` or `TRACER_DISABLED=1` in the environment turns everything off and nothing is written or sent. A host can choose to ignore `DO_NOT_TRACK` (`honor_do_not_track(false)`) and to make telemetry not optional (`force_consent(true)`: no switches, no notice); it must then say so in its own privacy text. `TRACER_DISABLED` cannot be ignored.
- **No user content.** Event names and property text are fixed strings compiled into the app. There is no API that accepts a file name, project name, document text or typed input.
- **No stable identity.** The install id is random, is not derived from hardware, and can be reset. The server never stores the IP address.
- **Visible.** With inspect mode on, everything that would be sent is also written to `outbox.jsonl` in the SDK's folder.
- **Forgettable.** "Forget me" deletes local data, including recent log lines and the last crash held in memory, asks the server to delete everything for the install, and issues a new id.

## Fields

| Field | Example | Category |
|---|---|---|
| App name, version, build id, process kind | `my-app`, `1.2.3`, `abc1234`, `gui` | all |
| Install id | random 32 hex characters | all |
| Session id | random 32 hex characters | all |
| Operating system name and version, CPU architecture | `macos`, `26.0`, `aarch64` | all |
| CPU model and core count, RAM size | `Example CPU`, `10`, `32768` MB | all |
| Graphics adapter name, vendor, backend, driver version | supplied by the host app | all |
| Locale | `en-US` | all |
| Session start, end, how it ended | `exited`, `crashed`, `abnormal` | usage |
| Session duration; focused, active and idle seconds | `62`, `50`, `40`, `22` | usage |
| Declared event name and category | `file.open`, `file` | usage |
| Event properties: booleans, numbers, fixed short strings | `format = "native"` | usage |
| The list of declared events and which are listened to | | usage |
| Frame-time histogram, slow and frozen frame counts | | performance |
| Startup time | `850` ms | performance |
| Process CPU percentage and memory use | `42.5`, `512` MB | performance |
| Host-defined counters and gauges | `audio.xruns = 2` | performance |
| Panic or error message, source location, thread name, backtrace | scrubbed, see below | crash |
| Hang duration | `2400` ms | crash |
| Child process label, exit code, signal | `exporter`, `134` | crash |
| Recent log lines (level, target, message) — only attached to a crash | scrubbed, see below | crash |
| Counts of data the client discarded | `queue_full = 4` | all |

## Scrubbing

Applied on the device, before anything is queued or written, to messages, backtraces and log lines:

- The user's home directory becomes `~`.
- The user's account name is removed where it appears in a path.
- Long values are truncated.

Log lines and error messages are written by the host app, so the host is responsible for not logging user content.

## Retention

The server keeps raw data for 90 days by default and daily totals indefinitely. Both are configurable.
