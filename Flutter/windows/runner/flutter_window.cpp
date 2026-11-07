#include "flutter_window.h"

#include <algorithm>
#include <optional>

#include "flutter/generated_plugin_registrant.h"

namespace {

// The smallest window the islands lay out in, as on the Mac (logical pixels).
constexpr int kMinimumWidth = 820;
constexpr int kMinimumHeight = 520;

// Centers |window| on its monitor's work area, as the Mac window centers
// itself, shrinking it to fit.
void Center(HWND window) {
  RECT bounds;
  MONITORINFO monitor = {};
  monitor.cbSize = static_cast<DWORD>(sizeof(monitor));
  if (!GetWindowRect(window, &bounds) ||
      !GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST),
                       &monitor)) {
    return;
  }
  const RECT& work = monitor.rcWork;
  const LONG width =
      std::min(bounds.right - bounds.left, work.right - work.left);
  const LONG height =
      std::min(bounds.bottom - bounds.top, work.bottom - work.top);
  SetWindowPos(window, nullptr,
               work.left + (work.right - work.left - width) / 2,
               work.top + (work.bottom - work.top - height) / 2, width, height,
               SWP_NOZORDER | SWP_NOACTIVATE);
}

}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  // Before the view takes the window's size; the window shows with the first
  // frame.
  Center(GetHandle());

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  channels_ = std::make_unique<WindowChannels>(
      flutter_controller_->engine()->messenger(), GetHandle());
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  // Before the engine its channels talk to.
  channels_ = nullptr;
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      if (flutter_controller_) {
        flutter_controller_->engine()->ReloadSystemFonts();
      }
      break;
    case WM_GETMINMAXINFO: {
      const int dpi = static_cast<int>(GetDpiForWindow(hwnd));
      auto* limits = reinterpret_cast<MINMAXINFO*>(lparam);
      limits->ptMinTrackSize.x =
          MulDiv(kMinimumWidth, dpi, USER_DEFAULT_SCREEN_DPI);
      limits->ptMinTrackSize.y =
          MulDiv(kMinimumHeight, dpi, USER_DEFAULT_SCREEN_DPI);
      return 0;
    }
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
