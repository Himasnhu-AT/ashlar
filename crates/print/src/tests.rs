#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const LPSTAT: &str = "printer Office_Laser is idle.  enabled since Mon 12 Oct 2026 09:14:02\n\
printer Label_Writer disabled since Tue 13 Oct 2026 11:00:00 -\n\
\treason unknown\n\
system default destination: Office_Laser\n";

fn memo_job() -> Job {
    Job { title: "memo.pdf".into(), ..Job::default() }
}

#[test]
fn lpstat_lists_queues_and_the_default() {
    let printers = parse_lpstat(LPSTAT);
    assert_eq!(
        printers,
        vec![
            Printer { name: "Office_Laser".into(), default: true, two_sided: None },
            Printer { name: "Label_Writer".into(), default: false, two_sided: None },
        ]
    );
    assert!(parse_lpstat("lpstat: No destinations added.\n").is_empty());
    assert!(parse_lpstat("Drucker Office_Laser ist im Leerlauf.\nStandardzielort: Office_Laser\n").is_empty());
}

#[test]
fn lpstat_runs_in_the_c_locale() {
    let command = lpstat_command();
    assert_eq!(command.get_program(), "lpstat");
    let envs: Vec<(String, String)> =
        command.get_envs().filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned()))).collect();
    assert!(envs.contains(&("LC_ALL".into(), "C".into())));
    assert!(envs.contains(&("LANG".into(), "C".into())));
    assert!(envs.iter().any(|(k, v)| k == "SOFTWARE" && !v.is_empty()));
}

#[test]
fn destinations_join_the_queues_once() {
    let queues = "printer Office_Laser is idle.\nsystem default destination: Office_Laser\n";
    let names = |list: Vec<Printer>| list.into_iter().map(|p| p.name).collect::<Vec<_>>();
    assert_eq!(names(printers_parsed(queues, Some("Office_Laser\nNetwork_Copier\n"))), vec!["Office_Laser", "Network_Copier"]);
    assert_eq!(names(printers_parsed(queues, None)), vec!["Office_Laser"]);
    let remote_default = printers_parsed("system default destination: Network_Copier\n", Some("Network_Copier\n"));
    assert_eq!(remote_default, vec![Printer { name: "Network_Copier".into(), default: true, two_sided: None }]);
    assert!(parse_lpstat_e("\n  \n").is_empty());
    assert_eq!(parse_lpstat_e(" Office_Laser \r\nNetwork_Copier\r\n"), vec!["Office_Laser", "Network_Copier"]);
}

#[test]
fn lp_arguments_follow_the_job() {
    let job = Job { printer: Some("Office_Laser".into()), copies: 3, collate: false, duplex: Some(Duplex::LongEdge), grayscale: true, ..memo_job() };
    let expected =
        "-d Office_Laser -n 3 -t memo.pdf -o collate=false -o sides=two-sided-long-edge -o print-color-mode=monochrome -o fit-to-page=false";
    assert_eq!(lp_args(&job).join(" "), expected);
    assert_eq!(lp_args(&Job::default()).first().map(String::as_str), Some("-n"));
    assert_eq!(lp_args(&Job { copies: 0, ..Job::default() })[1], "1");
    assert_eq!(lp_args(&Job { copies: u32::MAX, ..Job::default() })[1], "999");

    let default = lp_args(&Job::default()).join(" ");
    assert!(!default.contains("sides="));
    assert!(lp_args(&Job { duplex: Some(Duplex::Off), ..Job::default() }).contains(&"sides=one-sided".to_string()));
    let shrink = lp_args(&Job { scaling: Scaling::ShrinkToFit, ..Job::default() });
    assert!(shrink.contains(&"print-scaling=auto-fit".to_string()));
    assert!(!shrink.iter().any(|a| a.starts_with("fit-to-page=")));
}

#[test]
fn receipts_lose_the_stdin_file_count() {
    assert_eq!(job_message(b"request id is Office_Laser-17 (0 file(s))\n"), "request id is Office_Laser-17");
    assert_eq!(job_message(b"request id is Office_Laser-18 (1 file(s))"), "request id is Office_Laser-18 (1 file(s))");
    assert_eq!(job_message(b""), "");
}

#[test]
fn windows_printer_lines() {
    let out = "*Office Laser (Floor 2)\r\nКопир\r\n\r\n*Second Default\r\nPlain\r\n";
    let printers = parse_windows_printers(out);
    let names: Vec<_> = printers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Office Laser (Floor 2)", "Копир", "Second Default", "Plain"]);
    assert_eq!(printers.iter().map(|p| p.default).collect::<Vec<_>>(), vec![true, false, false, false]);
    assert!(printers.iter().all(|p| p.two_sided.is_none()));

    let printers = parse_windows_printers("*Duplex Laser\t2\r\nInkjet\t1\nOdd\tName\t2\n");
    assert_eq!(printers[0], Printer { name: "Duplex Laser".into(), default: true, two_sided: Some(true) });
    assert_eq!(printers[1], Printer { name: "Inkjet".into(), default: false, two_sided: Some(false) });
    assert_eq!(printers[2].name, "Odd\tName");
}

#[test]
fn windows_job_environment() {
    let path = std::path::Path::new("C:\\Temp\\ashlar-print-x.pdf");
    let job = Job {
        printer: Some("Front \"Desk\" Laser".into()),
        copies: 3,
        duplex: Some(Duplex::ShortEdge),
        grayscale: true,
        title: "Quarterly report".into(),
        ..Job::default()
    };
    let pairs = |list: Vec<(String, String)>| list.into_iter().collect::<std::collections::HashMap<_, _>>();
    let env = pairs(windows_env_vars(path, &job));
    assert_eq!(env.len(), 8);
    assert_eq!(env["ASHLAR_PRINT_FILE"], "C:\\Temp\\ashlar-print-x.pdf");
    assert_eq!(env["ASHLAR_PRINT_PRINTER"], "Front \"Desk\" Laser");
    assert_eq!(env["ASHLAR_PRINT_COPIES"], "3");
    assert_eq!(env["ASHLAR_PRINT_COLLATE"], "1");
    assert_eq!(env["ASHLAR_PRINT_DUPLEX"], "short-edge");
    assert_eq!(env["ASHLAR_PRINT_SCALING"], "exact");
    assert_eq!(env["ASHLAR_PRINT_GRAYSCALE"], "1");
    assert_eq!(env["ASHLAR_PRINT_TITLE"], "Quarterly report");

    let env = pairs(windows_env_vars(path, &Job::default()));
    assert_eq!((env["ASHLAR_PRINT_PRINTER"].as_str(), env["ASHLAR_PRINT_DUPLEX"].as_str()), ("", ""));
    let env = pairs(windows_env_vars(path, &Job { scaling: Scaling::ShrinkToFit, duplex: Some(Duplex::Off), ..Job::default() }));
    assert_eq!((env["ASHLAR_PRINT_SCALING"].as_str(), env["ASHLAR_PRINT_DUPLEX"].as_str()), ("shrink-to-fit", "off"));
    let env = pairs(windows_env_vars(path, &Job { duplex: Some(Duplex::LongEdge), ..Job::default() }));
    assert_eq!(env["ASHLAR_PRINT_DUPLEX"], "long-edge");
}

/// A PPD with the awkward parts real drivers ship: an installable-options group, multi-line
/// PostScript, translated lines, Latin-1 text, an option this crate handles itself, a default
/// that is not a choice, and a choice without a label.
const PPD: &[u8] = b"*PPD-Adobe: \"4.3\"\r\n\
*OpenGroup: InstallableOptions/Installed Options\r\n\
*OpenUI *Duplexer/Duplex unit: Boolean\r\n\
*DefaultDuplexer: False\r\n\
*Duplexer True/Installed: \"\"\r\n\
*Duplexer False/Not installed: \"\"\r\n\
*CloseUI: *Duplexer\r\n\
*CloseGroup: InstallableOptions\r\n\
*OpenGroup: MediaGroup/Media\r\n\
*OpenUI *InputSlot/Paper tray: PickOne\r\n\
*OrderDependency: 20 AnySetup *InputSlot\r\n\
*DefaultInputSlot: AutoSelect\r\n\
*InputSlot AutoSelect/Automatic: \"\r\n\
<</ManualFeed false>> setpagedevice\"\r\n\
*End\r\n\
*InputSlot Tray2/Tray 2: \"<</MediaPosition 2>> setpagedevice\"\r\n\
*de.InputSlot Tray2/Fach 2: \"\"\r\n\
*InputSlot Manual/Manual feed: \"\"\r\n\
*CloseUI: *InputSlot\r\n\
*OpenUI *PageSize/Page size: PickOne\r\n\
*DefaultPageSize: A4\r\n\
*PageSize A4/A4: \"\"\r\n\
*CloseUI: *PageSize\r\n\
*CloseGroup: MediaGroup\r\n\
*OpenUI *EFMediaType/Media type: PickOne\r\n\
*DefaultEFMediaType: Nope\r\n\
*EFMediaType Plain/Plain paper: \"\"\r\n\
*EFMediaType Heavy1/Kar\xADton m\xB2: \"\"\r\n\
*CloseUI: *EFMediaType\r\n\
*OpenUI *EFRaster: PickOne\r\n\
*DefaultEFRaster: Fast\r\n\
*EFRaster Fast: \"\"\r\n\
*EFRaster Fine/Fine detail: \"\"\r\n\
*CloseUI: *EFRaster\r\n";

#[test]
fn ppd_options_are_read_leniently() {
    let options = parse_ppd(&ppd_text(PPD));
    let keys: Vec<_> = options.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, vec!["InputSlot", "EFMediaType", "EFRaster"]);

    let tray = &options[0];
    assert_eq!((tray.label.as_str(), tray.group.as_str(), tray.default.as_str()), ("Paper tray", "Media", "AutoSelect"));
    let choices: Vec<(&str, &str)> = tray.choices.iter().map(|(k, l)| (k.as_str(), l.as_str())).collect();
    assert_eq!(choices, vec![("AutoSelect", "Automatic"), ("Tray2", "Tray 2"), ("Manual", "Manual feed")]);

    let media = &options[1];
    assert_eq!(media.choices[1].1, "Kar\u{AD}ton m\u{B2}");
    assert_eq!(media.default, "Plain");
    assert_eq!(media.group, "");

    let raster = &options[2];
    assert_eq!((raster.label.as_str(), raster.choices[0].1.as_str()), ("EFRaster", "Fast"));

    let unclosed = "*OpenUI *Broken/Broken: PickOne\n*DefaultBroken: A\n*Broken A/A: \"\"\n";
    assert!(parse_ppd(unclosed).is_empty());
}

#[test]
fn lpoptions_and_driver_options() {
    let out = "InputSlot/Paper tray: AutoSelect *Tray2 Manual\nEFMediaType/Media type: *Heavy1 Plain\nbroken line\nNoStar/No star: a b\n";
    assert_eq!(parse_lpoptions(out), vec![("InputSlot".into(), "Tray2".into()), ("EFMediaType".into(), "Heavy1".into())]);

    let job = Job {
        options: vec![
            ("InputSlot".into(), "Tray2".into()),
            ("PageSize".into(), "A4".into()),
            ("Bad Key".into(), "x".into()),
            ("EFMediaType".into(), "Heavy1".into()),
            ("Inject".into(), "a=b".into()),
        ],
        ..memo_job()
    };
    let args = lp_args(&job);
    let scaling = args.iter().position(|a| a == "fit-to-page=false").unwrap();
    assert_eq!(args[scaling + 1..].join(" "), "-o InputSlot=Tray2 -o EFMediaType=Heavy1");
    assert!(printer_options("../../etc/passwd").is_empty());
}

#[test]
fn previews_describe_the_command_without_running_it() {
    let job = memo_job();
    let preview = command_preview(&job).unwrap();
    match backend() {
        Some(Backend::Cups) => {
            assert_eq!(preview[0], "lp");
            assert_eq!(preview[1..], lp_args(&job)[..]);
        }
        _ => {
            assert_eq!(preview[0], "powershell");
            assert!(!preview.iter().any(|a| a.contains(".pdf")));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A stand-in for `lp`: this test binary, re-run with only `stand_in_lp` selected.

#[cfg(all(unix, not(target_arch = "wasm32")))]
mod spooler {
    use std::io::{Read, Write};
    use std::process::Command;

    use super::*;

    /// Names the stand-in's record directory, or `refuse`.
    const MODE: &str = "ASHLAR_PRINT_STAND_IN";
    const REFUSAL: &str = "lp: Office_Laser is not accepting jobs";

    fn stand_in(mode: &std::ffi::OsStr) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "tests::spooler::stand_in_lp", "--nocapture", "--"]).env(MODE, mode);
        command
    }

    /// Does nothing in a normal run. As the stand-in: records its `lp` arguments and standard
    /// input, or refuses without reading anything.
    #[test]
    fn stand_in_lp() {
        let Some(mode) = std::env::var_os(MODE) else { return };
        if mode == "refuse" {
            eprintln!("{REFUSAL}");
            std::process::exit(1);
        }
        let dir = std::path::PathBuf::from(mode);
        let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
        std::fs::write(dir.join("args"), args.join("\n")).unwrap();
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        std::fs::write(dir.join("stdin"), input).unwrap();
        std::io::stdout().write_all(b"request id is Office_Laser-42 (0 file(s))\n").unwrap();
        std::process::exit(0);
    }

    fn document() -> Vec<u8> {
        (0..3 * 1024 * 1024).map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect()
    }

    #[test]
    fn a_large_job_streams_to_the_spooler() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = document();
        let receipt = submit_via(stand_in(dir.path().as_os_str()), &pdf, &memo_job());
        assert!(receipt.is_ok(), "{receipt:?}");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert_eq!(args.split('\n').collect::<Vec<_>>().join(" "), "-n 1 -t memo.pdf -o collate=true -o fit-to-page=false");
        assert!(std::fs::read(dir.path().join("stdin")).unwrap() == pdf, "standard input differs from the document");
        let predictable = std::env::temp_dir().join(format!("ashlar-print-{}", std::process::id()));
        assert!(!predictable.exists() && !predictable.with_extension("pdf").exists());
    }

    #[test]
    fn a_refusal_reports_the_spooler_message() {
        let result = submit_via(stand_in("refuse".as_ref()), &document(), &memo_job());
        assert_eq!(result, Err(Error::Spool(REFUSAL.to_string())));
    }
}

// ---------------------------------------------------------------------------------------------
// Windows only.

#[cfg(windows)]
mod windows {
    use super::*;

    #[test]
    fn printers_are_well_formed() {
        let printers = printers();
        assert!(printers.iter().all(|p| !p.name.trim().is_empty() && p.two_sided.is_some()), "{printers:?}");
        assert!(printers.iter().filter(|p| p.default).count() <= 1);
    }

    #[test]
    fn powershell_is_found_by_absolute_path() {
        let command = powershell_command("exit 0");
        let program = std::path::PathBuf::from(command.get_program());
        assert!(program.is_absolute());
        assert!(program.to_string_lossy().replace('\\', "/").ends_with("System32/WindowsPowerShell/v1.0/powershell.exe"));
        assert!(program.exists());
    }

    #[test]
    fn the_print_script_parses() {
        let check = "$errors = $null; $null = [System.Management.Automation.Language.Parser]::ParseInput($env:ASHLAR_SCRIPT, [ref]$null, [ref]$errors); \
                     [Console]::Out.Write($errors.Count)";
        let out = powershell_command(check).env("ASHLAR_SCRIPT", WINDOWS_PRINT_SCRIPT).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
    }

    #[test]
    fn job_files_are_private_and_removed() {
        let a = windows_job_file(b"first").unwrap();
        let b = windows_job_file(b"second").unwrap();
        assert_ne!(a.to_path_buf(), b.to_path_buf());
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
        let gone = a.to_path_buf();
        drop(a);
        assert!(!gone.exists());
    }

    fn settings() -> serde_json::Value {
        let mut command = powershell_command(WINDOWS_PRINT_SCRIPT);
        command.env("ASHLAR_PRINT_TEST_SETTINGS", "1");
        let out = command.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    #[test]
    fn page_setup_arithmetic() {
        let v = settings();
        assert_eq!(v["LongEdge"], "Vertical");
        assert_eq!(v["ShortEdge"], "Horizontal");
        assert_eq!(v["Off"], "Simplex");
        assert_eq!(v["DefaultDuplex"], "Vertical");
        assert_eq!(v["ForcedSimplex"], "Simplex");
        assert_eq!(v["FallbackPaper"], "Driver Letter");
        let boxes: Vec<Vec<f64>> = serde_json::from_value(v["ShrinkBoxes"].clone()).unwrap();
        assert_eq!(boxes, vec![vec![0.0, 20.0, 800.0, 1000.0], vec![200.0, 270.0, 400.0, 500.0]]);
        let sheets = v["Sheets"].as_array().unwrap();
        let num = |s: &serde_json::Value, k: &str| s[k].as_f64().unwrap();
        assert_eq!((num(&sheets[0], "Width"), num(&sheets[0], "Height")), (850.0, 1100.0));
        assert_eq!(sheets[0]["Name"], "Driver Letter");
        assert_eq!(sheets[0]["Pixels"], serde_json::json!([1275, 1650]));
        assert_eq!(sheets[1]["Landscape"], true);
        assert_eq!(sheets[1]["Pixels"], serde_json::json!([1650, 1275]));
        assert_eq!((num(&sheets[2], "Width"), num(&sheets[2], "Height")), (827.0, 1169.0));
        assert_eq!(num(&sheets[3], "Height"), 1400.0);
        for sheet in sheets {
            assert_eq!(sheet["Margins"], serde_json::json!([0, 0, 0, 0]));
            assert_eq!((num(sheet, "X"), num(sheet, "Y")), (-25.0, -30.0));
        }
        assert_eq!((num(&sheets[0], "DrawWidth"), num(&sheets[1], "DrawWidth")), (850.0, 1100.0));
        let large: Vec<f64> = serde_json::from_value(v["LargePixels"].clone()).unwrap();
        assert!(large[0] * large[1] <= 64_000_000.0, "{large:?}");
        assert_eq!(v["InvalidRejected"], true);
    }

    /// A two-page PDF (portrait and landscape Letter) with one line on each page.
    fn two_page_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Contents 5 0 R >>".to_string(),
            "<< /Length 19 >>\nstream\n10 10 m 300 300 l S\nendstream".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
        pdf
    }

    fn powershell(script: &str) -> bool {
        powershell_command(script).output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn dry_run_then_a_paused_test_queue() {
        let pdf = two_page_pdf();
        let job = Job { printer: Some(format!("ashlar-missing-{}", std::process::id())), ..Job::default() };
        assert_eq!(windows_submit(&pdf, &job, &[("ASHLAR_PRINT_DRYRUN", "1")]), Ok("rendered 2 sheet(s)".to_string()));

        let unique = format!("ashlar-test-{}", std::process::id());
        let port = format!("{}\\{unique}.prn", std::env::temp_dir().display());
        let prepare = format!(
            "Add-PrinterPort -Name '{port}'; Add-Printer -Name '{unique}' -DriverName 'Generic / Text Only' -PortName '{port}'; \
             $null = (Get-CimInstance Win32_Printer -Filter \"Name='{unique}'\" | Invoke-CimMethod -MethodName Pause); \
             Add-Type -AssemblyName System.Drawing; $settings = New-Object System.Drawing.Printing.PrinterSettings; \
             $settings.PrinterName = '{unique}'; \
             for ($i = 0; -not $settings.IsValid -and $i -lt 75; $i++) {{ Start-Sleep -Milliseconds 200; $settings.PrinterName = '{unique}' }}; \
             if (-not $settings.IsValid) {{ exit 1 }}"
        );
        // The spooler can take a moment to list a new queue; the setup above waits until the same
        // check the print script makes passes, so the print below doesn't race it.
        let cleanup = format!(
            "Get-PrintJob -PrinterName '{unique}' -ErrorAction SilentlyContinue | Remove-PrintJob -ErrorAction SilentlyContinue; \
             Remove-Printer -Name '{unique}' -ErrorAction SilentlyContinue; Remove-PrinterPort -Name '{port}' -ErrorAction SilentlyContinue"
        );
        if !powershell(&prepare) {
            powershell(&cleanup);
            assert!(std::env::var("ASHLAR_PRINT_REQUIRE_QUEUE").as_deref() != Ok("1"), "the test queue could not be created");
            return;
        }
        let job = Job { printer: Some(unique.clone()), title: "ashlar print test".into(), ..Job::default() };
        let result = windows_submit(&pdf, &job, &[]);
        let count = powershell_command(&format!("[Console]::Out.Write(@(Get-PrintJob -PrinterName '{unique}').Count)")).output();
        powershell(&cleanup);
        assert!(result.is_ok(), "{result:?}");
        let count = count.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        assert_eq!(count, "1");
    }
}
