#include "window_channels.h"

#include <flutter/standard_method_codec.h>
#include <ole2.h>
#include <shellapi.h>
#include <shobjidl.h>
#include <wincodec.h>
#include <wrl/client.h>

#include <algorithm>
#include <cwchar>
#include <optional>
#include <utility>

#include "utils.h"

using Microsoft::WRL::ComPtr;

namespace {

std::wstring Utf16FromUtf8(const std::string& utf8) {
  if (utf8.empty()) {
    return std::wstring();
  }
  const int size = static_cast<int>(utf8.size());
  const int length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                         utf8.data(), size, nullptr, 0);
  if (length <= 0) {
    return std::wstring();
  }
  std::wstring utf16(static_cast<size_t>(length), L'\0');
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8.data(), size,
                      utf16.data(), length);
  return utf16;
}

std::string Win32Message(DWORD error) {
  wchar_t* buffer = nullptr;
  const DWORD length = FormatMessageW(
      FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
          FORMAT_MESSAGE_IGNORE_INSERTS,
      nullptr, error, 0, reinterpret_cast<wchar_t*>(&buffer), 0, nullptr);
  if (length == 0 || buffer == nullptr) {
    return "Windows error " + std::to_string(error);
  }
  std::string message = Utf8FromUtf16(buffer);
  LocalFree(buffer);
  while (!message.empty() &&
         (message.back() == '\n' || message.back() == '\r' ||
          message.back() == ' ' || message.back() == '.')) {
    message.pop_back();
  }
  return message;
}

flutter::EncodableValue PathList(const std::vector<std::string>& paths) {
  flutter::EncodableList list;
  list.reserve(paths.size());
  for (const std::string& path : paths) {
    list.emplace_back(path);
  }
  return flutter::EncodableValue(std::move(list));
}

// The regular files among |drop|'s paths: folders are not attached, as on the
// Mac.
std::vector<std::string> RegularFiles(HDROP drop) {
  std::vector<std::string> paths;
  const UINT count = DragQueryFileW(drop, 0xFFFFFFFF, nullptr, 0);
  for (UINT i = 0; i < count; ++i) {
    const UINT length = DragQueryFileW(drop, i, nullptr, 0);
    if (length == 0) {
      continue;
    }
    std::wstring path(length, L'\0');
    if (DragQueryFileW(drop, i, path.data(), length + 1) == 0) {
      continue;
    }
    const DWORD attributes = GetFileAttributesW(path.c_str());
    if (attributes == INVALID_FILE_ATTRIBUTES ||
        (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
      continue;
    }
    std::string utf8 = Utf8FromUtf16(path.c_str());
    if (!utf8.empty()) {
      paths.push_back(std::move(utf8));
    }
  }
  return paths;
}

// The files a drag carries (Explorer's, and every app that drags files).
std::vector<std::string> DraggedFiles(IDataObject* data) {
  if (data == nullptr) {
    return {};
  }
  FORMATETC format = {CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
  STGMEDIUM medium = {};
  if (FAILED(data->GetData(&format, &medium))) {
    return {};
  }
  std::vector<std::string> paths =
      RegularFiles(static_cast<HDROP>(medium.hGlobal));
  ReleaseStgMedium(&medium);
  return paths;
}

// The files chosen in the system's open dialog; HRESULT_FROM_WIN32(
// ERROR_CANCELLED) when it was cancelled.
HRESULT PickFiles(HWND owner, std::vector<std::string>* paths) {
  ComPtr<IFileOpenDialog> dialog;
  HRESULT hr = CoCreateInstance(CLSID_FileOpenDialog, nullptr,
                                CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&dialog));
  if (FAILED(hr)) {
    return hr;
  }
  FILEOPENDIALOGOPTIONS options = 0;
  hr = dialog->GetOptions(&options);
  if (SUCCEEDED(hr)) {
    hr = dialog->SetOptions(options | FOS_ALLOWMULTISELECT |
                            FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST |
                            FOS_PATHMUSTEXIST);
  }
  if (SUCCEEDED(hr)) {
    hr = dialog->SetTitle(L"Attach Files");
  }
  if (SUCCEEDED(hr)) {
    hr = dialog->Show(owner);
  }
  ComPtr<IShellItemArray> items;
  if (SUCCEEDED(hr)) {
    hr = dialog->GetResults(&items);
  }
  DWORD count = 0;
  if (SUCCEEDED(hr)) {
    hr = items->GetCount(&count);
  }
  if (FAILED(hr)) {
    return hr;
  }
  for (DWORD i = 0; i < count; ++i) {
    ComPtr<IShellItem> item;
    if (FAILED(items->GetItemAt(i, &item))) {
      continue;
    }
    PWSTR path = nullptr;
    if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path))) {
      paths->push_back(Utf8FromUtf16(path));
      CoTaskMemFree(path);
    }
  }
  return S_OK;
}

// A new file in the temporary folder for pasted pixels, as the Mac names them.
std::wstring PastedImagePath() {
  wchar_t folder[MAX_PATH + 1];
  const DWORD length = GetTempPathW(MAX_PATH + 1, folder);
  if (length == 0 || length > MAX_PATH) {
    return std::wstring();
  }
  GUID id;
  wchar_t text[40];
  if (FAILED(CoCreateGuid(&id)) || StringFromGUID2(id, text, 40) == 0) {
    return std::wstring();
  }
  // Without the braces.
  const std::wstring name(text + 1, wcslen(text) - 2);
  return std::wstring(folder, length) + L"Mailstream-pasted-" + name + L".png";
}

bool WriteBytes(const std::wstring& path, const void* bytes, SIZE_T size) {
  HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr,
                            CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (file == INVALID_HANDLE_VALUE) {
    return false;
  }
  const BYTE* cursor = static_cast<const BYTE*>(bytes);
  SIZE_T left = size;
  bool written = true;
  while (left > 0) {
    const DWORD chunk =
        static_cast<DWORD>(std::min<SIZE_T>(left, SIZE_T{1} << 30));
    DWORD done = 0;
    if (!WriteFile(file, cursor, chunk, &done, nullptr) || done == 0) {
      written = false;
      break;
    }
    cursor += done;
    left -= done;
  }
  CloseHandle(file);
  if (!written) {
    DeleteFileW(path.c_str());
  }
  return written;
}

// Encodes |bitmap| (the clipboard's CF_BITMAP, which Windows makes from the
// CF_DIB or CF_DIBV5 of screenshots and copied pixels) as an opaque PNG.
HRESULT WritePng(HBITMAP bitmap, const std::wstring& path) {
  ComPtr<IWICImagingFactory> factory;
  HRESULT hr = CoCreateInstance(CLSID_WICImagingFactory, nullptr,
                                CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory));
  ComPtr<IWICBitmap> source;
  if (SUCCEEDED(hr)) {
    // Screenshots' 32-bit pixels leave the alpha byte zero: ignore it.
    hr = factory->CreateBitmapFromHBITMAP(bitmap, nullptr,
                                          WICBitmapIgnoreAlpha, &source);
  }
  UINT width = 0;
  UINT height = 0;
  if (SUCCEEDED(hr)) {
    hr = source->GetSize(&width, &height);
  }
  ComPtr<IWICStream> stream;
  if (SUCCEEDED(hr)) {
    hr = factory->CreateStream(&stream);
  }
  if (SUCCEEDED(hr)) {
    hr = stream->InitializeFromFilename(path.c_str(), GENERIC_WRITE);
  }
  ComPtr<IWICBitmapEncoder> encoder;
  if (SUCCEEDED(hr)) {
    hr = factory->CreateEncoder(GUID_ContainerFormatPng, nullptr, &encoder);
  }
  if (SUCCEEDED(hr)) {
    hr = encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache);
  }
  ComPtr<IWICBitmapFrameEncode> frame;
  if (SUCCEEDED(hr)) {
    hr = encoder->CreateNewFrame(&frame, nullptr);
  }
  if (SUCCEEDED(hr)) {
    hr = frame->Initialize(nullptr);
  }
  if (SUCCEEDED(hr)) {
    hr = frame->SetSize(width, height);
  }
  // The encoder may answer the nearest format it writes; convert to that.
  WICPixelFormatGUID format = GUID_WICPixelFormat24bppBGR;
  if (SUCCEEDED(hr)) {
    hr = frame->SetPixelFormat(&format);
  }
  ComPtr<IWICFormatConverter> converter;
  if (SUCCEEDED(hr)) {
    hr = factory->CreateFormatConverter(&converter);
  }
  if (SUCCEEDED(hr)) {
    hr = converter->Initialize(source.Get(), format, WICBitmapDitherTypeNone,
                               nullptr, 0.0, WICBitmapPaletteTypeCustom);
  }
  if (SUCCEEDED(hr)) {
    hr = frame->WriteSource(converter.Get(), nullptr);
  }
  if (SUCCEEDED(hr)) {
    hr = frame->Commit();
  }
  if (SUCCEEDED(hr)) {
    hr = encoder->Commit();
  }
  return hr;
}

// Holds the clipboard open; another app may hold it for a moment.
class ClipboardLock {
 public:
  explicit ClipboardLock(HWND owner) {
    for (int attempt = 0; attempt < 10 && !open_; ++attempt) {
      open_ = OpenClipboard(owner) != FALSE;
      if (!open_) {
        Sleep(20);
      }
    }
  }
  ~ClipboardLock() {
    if (open_) {
      CloseClipboard();
    }
  }
  ClipboardLock(const ClipboardLock&) = delete;
  ClipboardLock& operator=(const ClipboardLock&) = delete;

  bool open() const { return open_; }

 private:
  bool open_ = false;
};

// The files on the clipboard: Explorer's copied files, else copied image
// pixels or a screenshot written to a temporary PNG. Text, even with a picture
// of itself beside it (cells copied from a spreadsheet), stays text: no files,
// so Flutter pastes it. Returns an error message when the clipboard cannot be
// read or the image cannot be saved.
std::optional<std::string> PastedFiles(HWND owner,
                                       std::vector<std::string>* paths) {
  ClipboardLock clipboard(owner);
  if (!clipboard.open()) {
    return "Another app is using the clipboard.";
  }
  if (IsClipboardFormatAvailable(CF_HDROP)) {
    if (HANDLE drop = GetClipboardData(CF_HDROP)) {
      *paths = RegularFiles(static_cast<HDROP>(drop));
    }
    if (!paths->empty()) {
      return std::nullopt;
    }
  }
  if (IsClipboardFormatAvailable(CF_UNICODETEXT)) {
    return std::nullopt;
  }
  // Browsers, Office and the Snipping Tool put a PNG beside the bitmap: it
  // keeps transparency.
  const UINT png = RegisterClipboardFormatW(L"PNG");
  const bool has_png = png != 0 && IsClipboardFormatAvailable(png);
  const bool has_bitmap = IsClipboardFormatAvailable(CF_BITMAP) != FALSE;
  if (!has_png && !has_bitmap) {
    return std::nullopt;
  }
  const std::wstring path = PastedImagePath();
  if (path.empty()) {
    return "Could not make a file for the pasted image.";
  }
  bool written = false;
  if (has_png) {
    if (HANDLE data = GetClipboardData(png)) {
      if (const void* bytes = GlobalLock(data)) {
        written = WriteBytes(path, bytes, GlobalSize(data));
        GlobalUnlock(data);
      }
    }
  }
  if (!written && has_bitmap) {
    HBITMAP bitmap = static_cast<HBITMAP>(GetClipboardData(CF_BITMAP));
    written = bitmap != nullptr && SUCCEEDED(WritePng(bitmap, path));
    if (!written) {
      DeleteFileW(path.c_str());
    }
  }
  if (!written) {
    return "Could not save the pasted image.";
  }
  paths->push_back(Utf8FromUtf16(path.c_str()));
  return std::nullopt;
}

}  // namespace

// The window's OLE drop target: tells the compose column when files are
// dragged over the window, and gives it their paths when they drop.
class FileDropTarget final : public IDropTarget {
 public:
  explicit FileDropTarget(WindowChannels* owner) : owner_(owner) {}

  FileDropTarget(const FileDropTarget&) = delete;
  FileDropTarget& operator=(const FileDropTarget&) = delete;

  // Called when the channels go away; OLE may still hold a reference.
  void Detach() { owner_ = nullptr; }

  // IUnknown:
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid,
                                           void** object) override {
    if (object == nullptr) {
      return E_POINTER;
    }
    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IDropTarget)) {
      *object = static_cast<IDropTarget*>(this);
      AddRef();
      return S_OK;
    }
    *object = nullptr;
    return E_NOINTERFACE;
  }

  ULONG STDMETHODCALLTYPE AddRef() override {
    return static_cast<ULONG>(InterlockedIncrement(&references_));
  }

  ULONG STDMETHODCALLTYPE Release() override {
    const LONG left = InterlockedDecrement(&references_);
    if (left == 0) {
      delete this;
    }
    return static_cast<ULONG>(left);
  }

  // IDropTarget:
  HRESULT STDMETHODCALLTYPE DragEnter(IDataObject* data,
                                      DWORD keys,
                                      POINTL point,
                                      DWORD* effect) override {
    has_files_ = owner_ != nullptr && owner_->accepts_files() &&
                 !DraggedFiles(data).empty();
    if (has_files_) {
      owner_->Hover(true);
    }
    *effect = has_files_ ? DROPEFFECT_COPY : DROPEFFECT_NONE;
    return S_OK;
  }

  HRESULT STDMETHODCALLTYPE DragOver(DWORD keys,
                                     POINTL point,
                                     DWORD* effect) override {
    const bool accepts =
        has_files_ && owner_ != nullptr && owner_->accepts_files();
    *effect = accepts ? DROPEFFECT_COPY : DROPEFFECT_NONE;
    return S_OK;
  }

  HRESULT STDMETHODCALLTYPE DragLeave() override {
    has_files_ = false;
    if (owner_ != nullptr) {
      owner_->Hover(false);
    }
    return S_OK;
  }

  HRESULT STDMETHODCALLTYPE Drop(IDataObject* data,
                                 DWORD keys,
                                 POINTL point,
                                 DWORD* effect) override {
    *effect = DROPEFFECT_NONE;
    has_files_ = false;
    if (owner_ == nullptr) {
      return S_OK;
    }
    owner_->Hover(false);
    if (!owner_->accepts_files()) {
      return S_OK;
    }
    const std::vector<std::string> paths = DraggedFiles(data);
    if (paths.empty()) {
      return S_OK;
    }
    owner_->Drop(paths);
    *effect = DROPEFFECT_COPY;
    return S_OK;
  }

 private:
  ~FileDropTarget() = default;

  WindowChannels* owner_;
  LONG references_ = 1;
  bool has_files_ = false;
};

WindowChannels::WindowChannels(flutter::BinaryMessenger* messenger,
                               HWND window)
    : window_(window) {
  const auto* codec = &flutter::StandardMethodCodec::GetInstance();
  files_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      messenger, "mailstream/files", codec);
  files_->SetMethodCallHandler([this](const Call& call, Result result) {
    HandleFiles(call, std::move(result));
  });
  shell_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      messenger, "mailstream/shell", codec);
  shell_->SetMethodCallHandler([this](const Call& call, Result result) {
    HandleShell(call, std::move(result));
  });

  // Drops need OLE on this (the window's) thread, in the apartment main()
  // made. The target sits on the top-level window: OLE looks for one from the
  // Flutter view under the pointer up through its parents.
  ole_initialized_ = SUCCEEDED(OleInitialize(nullptr));
  if (ole_initialized_) {
    drop_target_ = new FileDropTarget(this);
    if (FAILED(RegisterDragDrop(window_, drop_target_))) {
      drop_target_->Detach();
      drop_target_->Release();
      drop_target_ = nullptr;
    }
  }
}

WindowChannels::~WindowChannels() {
  files_->SetMethodCallHandler(nullptr);
  shell_->SetMethodCallHandler(nullptr);
  if (drop_target_ != nullptr) {
    RevokeDragDrop(window_);
    drop_target_->Detach();
    drop_target_->Release();
  }
  if (ole_initialized_) {
    OleUninitialize();
  }
}

void WindowChannels::Hover(bool hovering) {
  if (hovering == hovering_) {
    return;
  }
  hovering_ = hovering;
  files_->InvokeMethod("hover",
                       std::make_unique<flutter::EncodableValue>(hovering));
}

void WindowChannels::Drop(const std::vector<std::string>& paths) {
  files_->InvokeMethod("drop",
                       std::make_unique<flutter::EncodableValue>(PathList(paths)));
}

void WindowChannels::HandleFiles(const Call& call, Result result) {
  const std::string& method = call.method_name();
  if (method == "accept") {
    const bool* accept = std::get_if<bool>(call.arguments());
    accepts_files_ = accept != nullptr && *accept;
    if (!accepts_files_) {
      Hover(false);
    }
    result->Success();
  } else if (method == "editing") {
    // The Mac routes its Edit menu's Cmd+V to the compose column with it;
    // Windows' Ctrl+V reaches Flutter's keys directly.
    result->Success();
  } else if (method == "pick") {
    std::vector<std::string> paths;
    const HRESULT hr = PickFiles(window_, &paths);
    if (SUCCEEDED(hr) || hr == HRESULT_FROM_WIN32(ERROR_CANCELLED)) {
      result->Success(PathList(paths));
    } else {
      result->Error("pick", "Could not show the file dialog: " +
                                Win32Message(static_cast<DWORD>(hr)));
    }
  } else if (method == "pasted") {
    std::vector<std::string> paths;
    if (std::optional<std::string> error = PastedFiles(window_, &paths)) {
      result->Error("clipboard", *error);
    } else {
      result->Success(PathList(paths));
    }
  } else {
    result->NotImplemented();
  }
}

void WindowChannels::HandleShell(const Call& call, Result result) {
  if (call.method_name() != "open") {
    result->NotImplemented();
    return;
  }
  const std::string* target = std::get_if<std::string>(call.arguments());
  const std::wstring wide =
      target == nullptr ? std::wstring() : Utf16FromUtf8(*target);
  if (wide.empty()) {
    result->Error("open", "Nothing to open.");
    return;
  }
  // The default verb, as a double-click in Explorer: a file opens in its app,
  // a link in the browser.
  const INT_PTR code = reinterpret_cast<INT_PTR>(ShellExecuteW(
      window_, nullptr, wide.c_str(), nullptr, nullptr, SW_SHOWNORMAL));
  if (code > 32) {
    result->Success();
  } else if (code == SE_ERR_NOASSOC) {
    result->Error("open", "No app is set to open it.");
  } else {
    result->Error("open", Win32Message(GetLastError()));
  }
}
