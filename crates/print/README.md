# ashlar-print

Sends a print-ready PDF to the operating system's spooler and tells you which printers and
driver options exist. It is meant for document-oriented apps (layout, drawing, publishing tools)
that already produce PDF output. It depends on no UI framework and on no app. Minimum Rust 1.95.

```rust
let job = ashlar_print::Job { title: "Invoice 1042".into(), ..Default::default() };
let receipt = ashlar_print::submit(&pdf_bytes, &job)?;
```

## Who does what

The app is responsible for the document: generating the PDF, deciding whether the user may print
it, page ranges, imposition, printer's marks and bleed.

The crate is responsible for the spooler:

- discovering printers (`printers`) and, on CUPS, the driver options of a queue
  (`printer_options`, from its PPD and `lpoptions`);
- opening the driver's own preferences dialog on Windows (`open_printer_preferences`);
- submitting the job with copies, collation, duplex, colour mode and scaling (`submit`);
- describing the command it would run without running it (`command_preview`), for logs and
  support reports.

## Platforms

- **CUPS (Linux, macOS, BSD).** Discovery runs `lpstat` in the C locale. The PDF is streamed to
  `lp` on standard input, so nothing is written to disk. `Scaling::Exact` sends
  `fit-to-page=false`; `Scaling::ShrinkToFit` sends `print-scaling=auto-fit`. Driver options
  from `printer_options` are forwarded with `-o` when they are plain PPD keywords.
- **Windows.** Uses only what ships with Windows: Windows PowerShell, the WinRT PDF renderer and
  System.Drawing printing. Each page is rasterized at the device resolution and printed on paper
  that matches its size and orientation, so mixed page sizes work in one job. `Exact` prints at
  physical size and reports an error when the printer cannot use a page's sheet size;
  `ShrinkToFit` scales pages down (never up) into the printable area, falling back to the
  default paper. `Duplex::LongEdge` maps to the driver's vertical duplex, `ShortEdge` to
  horizontal. Driver-specific settings stay in the printer preferences dialog.
- **Other targets (including wasm32).** `printers` is empty and `submit` returns an error that
  suggests saving the PDF instead.

## Behaviour worth knowing

- Temporary files: only on Windows, where the renderer needs a file. It gets a random name, is
  created exclusively in the user's temp directory and is removed as soon as the job has been
  handed over (or failed). The CUPS path never writes the document to disk.
- The API is synchronous and can block for seconds: call it from a worker thread.
- A receipt is whatever text the backend reports (for CUPS, the `lp` request line); do not parse
  it.
- `duplex: None` keeps the queue's default; `Some(Duplex::Off)` forces one-sided printing.
- `Scaling::Exact` is the default. A page whose size the printer cannot produce is reported, not
  silently scaled.
- Rasters are bounded to 64 million pixels per sheet and 32768 pixels per side; sheets larger
  than 128 inches on a side are rejected.
- `Printer::two_sided` is reported on Windows and unknown (`None`) on CUPS. A dialog can warn
  when a duplex job goes to a printer that reports no duplex support.
- Not goals: PostScript output, colour separations, trapping, colour-managed rasterization, or
  passing the PDF through untouched on Windows.

## Tests

`cargo test -p ashlar-print` covers, on every platform, the parsers (`lpstat`, `lpstat -e`,
PPD files, `lpoptions`, the Windows printer listing), `lp` argument building, the Windows job
environment and `command_preview`. On Unix it also submits a 3 MiB job to a stand-in spooler (the
test binary itself) and checks the arguments, the exact bytes received and that refusals surface
the spooler's own message. On Windows it also checks the embedded script for parser errors, the
page-setup arithmetic, the temporary job files, a dry-run render of a two-page PDF and, when a
paused test queue on a local port can be created, one real submission. When that queue cannot be
created the real submission is skipped; set `ASHLAR_PRINT_REQUIRE_QUEUE=1` (for example in a
Windows CI job) to make that a failure instead.

The repository gate in `.github/workflows/ci.yml` runs fmt, clippy and the tests of every crate,
this one included, on Linux, macOS and Windows. It does not set `ASHLAR_PRINT_REQUIRE_QUEUE`.

None of this proves physical output: paper handling, margins and duplex on real hardware still
need a test print.
