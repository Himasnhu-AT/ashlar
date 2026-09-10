# Prints a PDF through the Windows print system: each page is rendered by the system PDF
# renderer (WinRT) and drawn with System.Drawing, with paper and orientation chosen per page.
#
# Input comes only from the environment (set by windows_submit in lib.rs):
#   ASHLAR_PRINT_FILE       path of the PDF
#   ASHLAR_PRINT_PRINTER    queue name, empty for the system default
#   ASHLAR_PRINT_COPIES     copy count (clamped here to 1..32767)
#   ASHLAR_PRINT_COLLATE    1 or 0
#   ASHLAR_PRINT_DUPLEX     empty (driver default), off, long-edge or short-edge
#   ASHLAR_PRINT_SCALING    exact or shrink-to-fit
#   ASHLAR_PRINT_GRAYSCALE  1 or 0
#   ASHLAR_PRINT_TITLE      document name in the queue
# Test switches: ASHLAR_PRINT_DRYRUN=1 renders without printing, ASHLAR_PRINT_TEST_SETTINGS=1
# prints the page-setup arithmetic as JSON.
#
# Success: one status line on stdout, exit code 0. Failure: an exception (stop-on-error), so the
# message lands on stderr with a non-zero exit code.

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
Add-Type -AssemblyName System.Drawing

# --- page setup arithmetic (pure; checked by the test-settings mode) --------------------------

# Largest sheet side accepted, in DIPs (96 per inch): 128 inches.
$MaxSheetDips = 12288.0

function Resolve-Duplex([string]$Value, $Current) {
    if ([string]::IsNullOrEmpty($Value)) { return $Current }
    switch ($Value) {
        'long-edge' { return [System.Drawing.Printing.Duplex]::Vertical }
        'short-edge' { return [System.Drawing.Printing.Duplex]::Horizontal }
        default { return [System.Drawing.Printing.Duplex]::Simplex }
    }
}

# A PDF page size in DIPs as a sheet in hundredths of an inch.
function Get-Sheet([double]$WidthDips, [double]$HeightDips) {
    foreach ($side in @($WidthDips, $HeightDips)) {
        if ([double]::IsNaN($side) -or [double]::IsInfinity($side) -or $side -le 0 -or $side -gt $MaxSheetDips) {
            throw "The document has invalid or oversized sheet dimensions ($WidthDips x $HeightDips DIPs)."
        }
    }
    $w = $WidthDips * 100.0 / 96.0
    $h = $HeightDips * 100.0 / 96.0
    $paperW = [Math]::Max(1, [int][Math]::Round([Math]::Min($w, $h)))
    $paperH = [Math]::Max(1, [int][Math]::Round([Math]::Max($w, $h)))
    return @{ Width = $w; Height = $h; Landscape = ($w -gt $h); PaperWidth = $paperW; PaperHeight = $paperH }
}

# The paper and orientation for a sheet: a driver paper of the same size, else (shrink-to-fit)
# the printer's default paper, else a custom paper of exactly the sheet's size.
function Get-PageChoice($Sheet, $Papers, [string]$Scaling, $Fallback) {
    $paper = $null
    foreach ($candidate in $Papers) {
        if ([Math]::Abs($candidate.Width - $Sheet.PaperWidth) -le 1 -and [Math]::Abs($candidate.Height - $Sheet.PaperHeight) -le 1) {
            $paper = $candidate
            break
        }
    }
    if ($null -eq $paper -and $Scaling -eq 'shrink-to-fit') {
        if ($null -eq $Fallback) { throw 'The printer reports no default paper to shrink the page onto.' }
        $paper = $Fallback
    }
    if ($null -eq $paper) {
        $paper = New-Object System.Drawing.Printing.PaperSize 'PDF sheet', $Sheet.PaperWidth, $Sheet.PaperHeight
    }
    return @{ Paper = $paper; Landscape = $Sheet.Landscape; Margins = (New-Object System.Drawing.Printing.Margins 0, 0, 0, 0) }
}

function Set-PageChoice($PageSettings, $Choice) {
    $PageSettings.PaperSize = $Choice.Paper
    $PageSettings.Landscape = $Choice.Landscape
    $PageSettings.Margins = $Choice.Margins
}

# Physical size: the printable-area origin is moved back to the sheet's corner.
function Get-ExactBox($Sheet, [double]$HardX, [double]$HardY) {
    return @(-$HardX, -$HardY, $Sheet.Width, $Sheet.Height)
}

# Scaled down (never up) and centred in the printable area, in printable-area coordinates.
function Get-ShrinkBox($Sheet, [double]$AreaX, [double]$AreaY, [double]$AreaW, [double]$AreaH, [double]$HardX, [double]$HardY) {
    foreach ($v in @($AreaX, $AreaY, $AreaW, $AreaH, $HardX, $HardY)) {
        if ([double]::IsNaN($v) -or [double]::IsInfinity($v)) { throw 'The printer reported an invalid printable area.' }
    }
    if ($AreaW -le 0 -or $AreaH -le 0) { throw 'The printer reported an empty printable area.' }
    $scale = [Math]::Min(1.0, [Math]::Min($AreaW / $Sheet.Width, $AreaH / $Sheet.Height))
    $w = $Sheet.Width * $scale
    $h = $Sheet.Height * $scale
    $x = $AreaX + ($AreaW - $w) / 2.0 - $HardX
    $y = $AreaY + ($AreaH - $h) / 2.0 - $HardY
    return @($x, $y, $w, $h)
}

# Raster size for a sheet (hundredths of an inch) at a dpi, bounded to 32768 pixels per side and
# 64 million pixels in all. The physical size it is drawn at does not change.
function Get-RenderSize([double]$Width, [double]$Height, [double]$Dpi) {
    $dpi = [Math]::Min(600.0, [Math]::Max(72.0, $Dpi))
    $w = [Math]::Ceiling($Width / 100.0 * $dpi)
    $h = [Math]::Ceiling($Height / 100.0 * $dpi)
    $scale = [Math]::Min(1.0, [Math]::Min(32768.0 / [Math]::Max($w, $h), [Math]::Sqrt(64000000.0 / ($w * $h))))
    return @([Math]::Max(1, [int][Math]::Floor($w * $scale)), [Math]::Max(1, [int][Math]::Floor($h * $scale)))
}

if ($env:ASHLAR_PRINT_TEST_SETTINGS -eq '1') {
    $letter = New-Object System.Drawing.Printing.PaperSize 'Driver Letter', 850, 1100
    $papers = @($letter)
    $sheets = @()
    foreach ($size in @(@(816, 1056), @(1056, 816), @(793.92, 1122.24), @(816, 1344))) {
        $sheet = Get-Sheet $size[0] $size[1]
        $choice = Get-PageChoice $sheet $papers 'exact' $letter
        $box = Get-ExactBox $sheet 25 30
        $m = $choice.Margins
        $sheets += [ordered]@{
            Width = $choice.Paper.Width; Height = $choice.Paper.Height; Name = $choice.Paper.PaperName; Landscape = $choice.Landscape
            Margins = @($m.Left, $m.Right, $m.Top, $m.Bottom); X = $box[0]; Y = $box[1]; DrawWidth = $box[2]; DrawHeight = $box[3]
            Pixels = (Get-RenderSize $sheet.Width $sheet.Height 150)
        }
    }
    $large = Get-Sheet 12288 12288
    $invalid = $false
    try { $null = Get-Sheet ([double]::NaN) 100 } catch { $invalid = $true }
    $fallback = Get-PageChoice (Get-Sheet 1536 1920) $papers 'shrink-to-fit' $letter
    $vertical = [System.Drawing.Printing.Duplex]::Vertical
    $result = [ordered]@{
        Sheets = $sheets
        LongEdge = (Resolve-Duplex 'long-edge' $null).ToString()
        ShortEdge = (Resolve-Duplex 'short-edge' $null).ToString()
        Off = (Resolve-Duplex 'off' $null).ToString()
        DefaultDuplex = (Resolve-Duplex '' $vertical).ToString()
        ForcedSimplex = (Resolve-Duplex 'off' $vertical).ToString()
        FallbackPaper = $fallback.Paper.PaperName
        # Two arrays of four numbers each (the comma keeps them apart).
        ShrinkBoxes = @((Get-ShrinkBox (Get-Sheet 1536 1920) 25 30 800 1040 25 30), (Get-ShrinkBox (Get-Sheet 384 480) 25 30 800 1040 25 30))
        LargePixels = (Get-RenderSize $large.Width $large.Height 600)
        InvalidRejected = $invalid
    }
    [Console]::Out.WriteLine(($result | ConvertTo-Json -Compress -Depth 5))
    exit 0
}

# --- PDF rendering (WinRT) -------------------------------------------------------------------

try {
    $null = [Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType = WindowsRuntime]
    $null = [Windows.Data.Pdf.PdfPageRenderOptions, Windows.Data.Pdf, ContentType = WindowsRuntime]
    $null = [Windows.Storage.Streams.InMemoryRandomAccessStream, Windows.Storage.Streams, ContentType = WindowsRuntime]
} catch {
    throw 'This version of Windows cannot render PDF documents for printing.'
}
$null = [Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
Add-Type -AssemblyName System.Runtime.WindowsRuntime

$extensions = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 }
$asTaskOperation = $extensions | Where-Object { $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
$asTaskAction = $extensions | Where-Object { $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction' } | Select-Object -First 1

function Wait-Operation($Operation, [Type]$ResultType) {
    $task = $asTaskOperation.MakeGenericMethod($ResultType).Invoke($null, @($Operation))
    $null = $task.Wait(-1)
    return $task.Result
}

function Wait-Action($Action) {
    $task = $asTaskAction.Invoke($null, @($Action))
    $null = $task.Wait(-1)
}

# One page as a bitmap of the given pixel size.
function Get-PageBitmap($Page, [int[]]$Pixels) {
    $options = New-Object Windows.Data.Pdf.PdfPageRenderOptions
    $options.DestinationWidth = [uint32]$Pixels[0]
    $options.DestinationHeight = [uint32]$Pixels[1]
    $stream = New-Object Windows.Storage.Streams.InMemoryRandomAccessStream
    try {
        Wait-Action ($Page.RenderToStreamAsync($stream, $options))
        $stream.Seek(0)
        $net = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($stream)
        try {
            $decoded = [System.Drawing.Image]::FromStream($net)
            try { return New-Object System.Drawing.Bitmap $decoded } finally { $decoded.Dispose() }
        } finally { $net.Dispose() }
    } finally { $stream.Dispose() }
}

$file = Wait-Operation ([Windows.Storage.StorageFile]::GetFileFromPathAsync($env:ASHLAR_PRINT_FILE)) ([Windows.Storage.StorageFile])
$pdf = Wait-Operation ([Windows.Data.Pdf.PdfDocument]::LoadFromFileAsync($file)) ([Windows.Data.Pdf.PdfDocument])
$pageCount = [int]$pdf.PageCount
if ($pageCount -lt 1) { throw 'The document has no pages to print.' }

function Get-PageSheet([int]$Index) {
    $page = $pdf.GetPage([uint32]$Index)
    try { return Get-Sheet $page.Size.Width $page.Size.Height } finally { $page.Dispose() }
}

if ($env:ASHLAR_PRINT_DRYRUN -eq '1') {
    for ($i = 0; $i -lt $pageCount; $i++) {
        $page = $pdf.GetPage([uint32]$i)
        try {
            $sheet = Get-Sheet $page.Size.Width $page.Size.Height
            $pixels = Get-RenderSize $sheet.Width $sheet.Height 150
            $bitmap = Get-PageBitmap $page $pixels
            try {
                if ($bitmap.Width -ne $pixels[0] -or $bitmap.Height -ne $pixels[1]) {
                    throw "Page $($i + 1) rendered at $($bitmap.Width)x$($bitmap.Height) instead of $($pixels[0])x$($pixels[1])."
                }
            } finally { $bitmap.Dispose() }
        } finally { $page.Dispose() }
    }
    [Console]::Out.WriteLine("rendered $pageCount sheet(s)")
    exit 0
}

# --- the print job ---------------------------------------------------------------------------

$script:scaling = $env:ASHLAR_PRINT_SCALING
$script:next = 0
$document = New-Object System.Drawing.Printing.PrintDocument
try {
    $document.DocumentName = $env:ASHLAR_PRINT_TITLE
    $printerName = $env:ASHLAR_PRINT_PRINTER
    if (-not [string]::IsNullOrEmpty($printerName)) {
        $document.PrinterSettings.PrinterName = $printerName
        if (-not $document.PrinterSettings.IsValid) { throw "The printer '$printerName' is not available." }
    }
    $copies = 1
    if (-not [int]::TryParse($env:ASHLAR_PRINT_COPIES, [ref]$copies)) { $copies = 1 }
    $document.PrinterSettings.Copies = [int16][Math]::Min(32767, [Math]::Max(1, $copies))
    $document.PrinterSettings.Collate = ($env:ASHLAR_PRINT_COLLATE -eq '1')
    $document.PrinterSettings.Duplex = Resolve-Duplex $env:ASHLAR_PRINT_DUPLEX $document.PrinterSettings.Duplex
    if ($env:ASHLAR_PRINT_GRAYSCALE -eq '1') { $document.DefaultPageSettings.Color = $false }
    $script:fallbackPaper = $document.DefaultPageSettings.PaperSize
    $script:papers = @($document.PrinterSettings.PaperSizes)
    $document.OriginAtMargins = $false
    $document.PrintController = New-Object System.Drawing.Printing.StandardPrintController

    # Before each page: its own paper and orientation, so one job can mix sizes.
    $document.add_QueryPageSettings({
            param($sender, $e)
            $sheet = Get-PageSheet $script:next
            Set-PageChoice $e.PageSettings (Get-PageChoice $sheet $script:papers $script:scaling $script:fallbackPaper)
        })

    $document.add_PrintPage({
            param($sender, $e)
            $index = $script:next
            $page = $pdf.GetPage([uint32]$index)
            try {
                $sheet = Get-Sheet $page.Size.Width $page.Size.Height
                $hardX = [double]$e.PageSettings.HardMarginX
                $hardY = [double]$e.PageSettings.HardMarginY
                if ($script:scaling -eq 'shrink-to-fit') {
                    $area = $e.Graphics.VisibleClipBounds
                    $box = Get-ShrinkBox $sheet $hardX $hardY ([double]$area.Width) ([double]$area.Height) $hardX $hardY
                } else {
                    $bounds = $e.PageBounds
                    if ([Math]::Abs($bounds.Width - $sheet.Width) -gt 2 -or [Math]::Abs($bounds.Height - $sheet.Height) -gt 2) {
                        throw "The printer cannot use the sheet size of page $($index + 1). Select matching paper in the printer preferences, or shrink pages to fit."
                    }
                    $box = Get-ExactBox $sheet $hardX $hardY
                }
                $bitmap = Get-PageBitmap $page (Get-RenderSize $sheet.Width $sheet.Height ([double]$e.Graphics.DpiX))
                try {
                    $target = New-Object System.Drawing.RectangleF ([single]$box[0]), ([single]$box[1]), ([single]$box[2]), ([single]$box[3])
                    $e.Graphics.DrawImage($bitmap, $target)
                } finally { $bitmap.Dispose() }
            } finally { $page.Dispose() }
            $script:next = $index + 1
            $e.HasMorePages = ($script:next -lt $pageCount)
        })

    $document.Print()
    [Console]::Out.WriteLine("spooled $pageCount sheet(s)")
} finally {
    $document.Dispose()
}
