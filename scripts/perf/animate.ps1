# Synthetic desktop workload for replay profiling.
# Opens a WPF window with a continuously moving shape rendered at the display
# refresh rate (DesiredFrameRate 240, capped by the compositor), so DWM - and
# therefore Windows Graphics Capture - produces a new desktop frame every
# refresh. Closes itself after -Seconds.
param([int]$Seconds = 60, [int]$Width = 900, [int]$Height = 600)

Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase

$window = New-Object System.Windows.Window
$window.Title = 'clipuh perf workload'
$window.Width = $Width
$window.Height = $Height
$window.Left = 40
$window.Top = 40
$window.Topmost = $true
$canvas = New-Object System.Windows.Controls.Canvas
$canvas.Background = [System.Windows.Media.Brushes]::Black
$rect = New-Object System.Windows.Shapes.Rectangle
$rect.Width = 160
$rect.Height = 160
$rect.Fill = [System.Windows.Media.Brushes]::OrangeRed
$canvas.Children.Add($rect) | Out-Null
$window.Content = $canvas

$animation = New-Object System.Windows.Media.Animation.DoubleAnimation
$animation.From = 0
$animation.To = $Width - 200
$animation.Duration = [System.Windows.Duration]::new([TimeSpan]::FromSeconds(1.3))
$animation.AutoReverse = $true
$animation.RepeatBehavior = [System.Windows.Media.Animation.RepeatBehavior]::Forever
[System.Windows.Media.Animation.Timeline]::SetDesiredFrameRate($animation, 240)

$timer = New-Object System.Windows.Threading.DispatcherTimer
$timer.Interval = [TimeSpan]::FromSeconds($Seconds)
$timer.Add_Tick({ $window.Close() })

$window.Add_Loaded({
    $rect.BeginAnimation([System.Windows.Controls.Canvas]::LeftProperty, $animation)
    $timer.Start()
})
$window.ShowDialog() | Out-Null
