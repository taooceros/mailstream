#ifndef RUNNER_WINDOW_CHANNELS_H_
#define RUNNER_WINDOW_CHANNELS_H_

#include <flutter/binary_messenger.h>
#include <flutter/encodable_value.h>
#include <flutter/method_channel.h>
#include <windows.h>

#include <memory>
#include <string>
#include <vector>

class FileDropTarget;

// The window's channels to the Dart UI on Windows, the counterparts of
// macos/Runner/MainFlutterWindow.swift:
//
// `mailstream/files` (the compose column, lib/ui/compose.dart): `accept` turns
// file drops on the window on or off, `editing` says whether the compose
// column has focus, `pick` answers the files chosen in an open dialog and
// `pasted` the files on the clipboard (copied image pixels and screenshots
// written to a temporary PNG first). The window calls `hover` while files are
// dragged over it and `drop` with their paths.
//
// `mailstream/shell` (lib/state/mail_state.dart): `open` opens a file in its
// app or a link in the browser, through the shell's default verb.
class WindowChannels {
 public:
  // |window| is the top-level window: it owns the dialogs and the clipboard and
  // takes the drops.
  WindowChannels(flutter::BinaryMessenger* messenger, HWND window);
  ~WindowChannels();

  WindowChannels(const WindowChannels&) = delete;
  WindowChannels& operator=(const WindowChannels&) = delete;

  // Called by the drop target.
  bool accepts_files() const { return accepts_files_; }
  void Hover(bool hovering);
  void Drop(const std::vector<std::string>& paths);

 private:
  using Call = flutter::MethodCall<flutter::EncodableValue>;
  using Result = std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>>;

  void HandleFiles(const Call& call, Result result);
  void HandleShell(const Call& call, Result result);

  HWND window_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> files_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> shell_;
  FileDropTarget* drop_target_ = nullptr;
  bool ole_initialized_ = false;
  bool accepts_files_ = false;
  bool hovering_ = false;
};

#endif  // RUNNER_WINDOW_CHANNELS_H_
