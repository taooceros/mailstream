#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>

#include "flutter_window.h"
#include "utils.h"

namespace {

// The engine (MailstreamKit.dll, loaded by the Dart bindings) is main-actor Swift code. On
// Windows the main actor is libdispatch's main queue, which nothing drains by itself: on the Mac
// the run loop does. This loop waits on the queue's event alongside the window messages and
// drains it on this thread, the same way CoreFoundation's run loop does it on Windows.
using MainQueueHandle = HANDLE (*)();
using MainQueueDrain = void (*)(void*);

struct MainQueue {
  MainQueueHandle handle = nullptr;
  MainQueueDrain drain = nullptr;

  // dispatch.dll is part of the Swift runtime installed next to the executable.
  static MainQueue Load() {
    MainQueue queue;
    HMODULE dispatch = ::LoadLibraryW(L"dispatch.dll");
    if (dispatch) {
      queue.handle = reinterpret_cast<MainQueueHandle>(
          ::GetProcAddress(dispatch, "_dispatch_get_main_queue_handle_4CF"));
      queue.drain = reinterpret_cast<MainQueueDrain>(
          ::GetProcAddress(dispatch, "_dispatch_main_queue_callback_4CF"));
    }
    return queue;
  }
};

}  // namespace

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1240, 800);
  if (!window.Create(L"Mailstream", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  MainQueue main_queue = MainQueue::Load();
  HANDLE queue_event = main_queue.handle && main_queue.drain ? main_queue.handle() : nullptr;

  ::MSG msg;
  bool running = true;
  while (running) {
    DWORD count = queue_event ? 1 : 0;
    DWORD woke = ::MsgWaitForMultipleObjectsEx(count, &queue_event, INFINITE, QS_ALLINPUT,
                                               MWMO_INPUTAVAILABLE);
    if (queue_event && woke == WAIT_OBJECT_0) {
      main_queue.drain(nullptr);
    }
    while (::PeekMessage(&msg, nullptr, 0, 0, PM_REMOVE)) {
      if (msg.message == WM_QUIT) {
        running = false;
        break;
      }
      ::TranslateMessage(&msg);
      ::DispatchMessage(&msg);
    }
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
