// Wide (W) Windows types throughout: DnsQueryEx answers with wide strings.
#define UNICODE
#define _UNICODE
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <dpapi.h>
#include <shellapi.h>
#include <windns.h>
#include <wincodec.h>
#include <objbase.h>
#include <icu.h>
#include <stdlib.h>
#include <string.h>

#include "CWindows.h"

void ms_free(void *pointer) { free(pointer); }

/// `text` (UTF-8) as a new UTF-16 string, or NULL.
static wchar_t *wide(const char *text) {
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, NULL, 0);
    if (count <= 0) return NULL;
    wchar_t *out = malloc(sizeof(wchar_t) * (size_t)count);
    if (out && MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, out, count) != count) {
        free(out);
        return NULL;
    }
    return out;
}

/// `count` UTF-16 units (-1: up to the terminator) as new NUL-terminated UTF-8; `length` without it.
static char *narrow(const wchar_t *text, int count, size_t *length) {
    int bytes = WideCharToMultiByte(CP_UTF8, 0, text, count, NULL, 0, NULL, NULL);
    if (bytes < 0 || (bytes == 0 && count != 0)) return NULL;
    char *out = malloc((size_t)bytes + 1);
    if (!out) return NULL;
    if (bytes > 0 && WideCharToMultiByte(CP_UTF8, 0, text, count, out, bytes, NULL, NULL) != bytes) {
        free(out);
        return NULL;
    }
    out[bytes] = 0;
    size_t used = (size_t)bytes;
    if (count == -1 && used > 0) used -= 1;  // the terminator came along
    if (length) *length = used;
    return out;
}

/// Joins the COM apartment the calling thread has, else the multithreaded one; true when this
/// call initialized COM and must uninitialize it.
static BOOL enter_com(HRESULT *status) {
    HRESULT hr = CoInitializeEx(NULL, COINIT_MULTITHREADED);
    *status = hr == RPC_E_CHANGED_MODE ? S_OK : hr;
    return SUCCEEDED(hr);
}

// MARK: - Data protection

static int crypt(BOOL protect, const uint8_t *data, size_t length, uint8_t **out, size_t *out_length) {
    if (length > MAXDWORD) return ERROR_INVALID_PARAMETER;
    DATA_BLOB input = {(DWORD)length, (BYTE *)data};
    DATA_BLOB output = {0, NULL};
    BOOL done = protect
        ? CryptProtectData(&input, L"Mailstream sign-ins", NULL, NULL, NULL, CRYPTPROTECT_UI_FORBIDDEN, &output)
        : CryptUnprotectData(&input, NULL, NULL, NULL, NULL, CRYPTPROTECT_UI_FORBIDDEN, &output);
    if (!done) return (int)GetLastError();
    *out = malloc(output.cbData ? output.cbData : 1);
    if (!*out) {
        SecureZeroMemory(output.pbData, output.cbData);
        LocalFree(output.pbData);
        return ERROR_OUTOFMEMORY;
    }
    memcpy(*out, output.pbData, output.cbData);
    *out_length = output.cbData;
    SecureZeroMemory(output.pbData, output.cbData);
    LocalFree(output.pbData);
    return 0;
}

int ms_protect(const uint8_t *data, size_t length, uint8_t **out, size_t *out_length) {
    return crypt(TRUE, data, length, out, out_length);
}

int ms_unprotect(const uint8_t *data, size_t length, uint8_t **out, size_t *out_length) {
    return crypt(FALSE, data, length, out, out_length);
}

// MARK: - Shell

int ms_open_url(const char *url) {
    wchar_t *target = wide(url);
    if (!target) return ERROR_INVALID_PARAMETER;
    HINSTANCE result = ShellExecuteW(NULL, L"open", target, NULL, NULL, SW_SHOWNORMAL);
    free(target);
    // ShellExecute reports success as a value above 32.
    return (INT_PTR)result > 32 ? 0 : (int)(INT_PTR)result;
}

// MARK: - Loopback TCP

static int start_winsock(void) {
    static volatile LONG started = 0;
    if (InterlockedCompareExchange(&started, 1, 0) == 0) {
        WSADATA data;
        int error = WSAStartup(MAKEWORD(2, 2), &data);
        if (error) {
            InterlockedExchange(&started, 0);
            return error;
        }
    }
    return 0;
}

static int non_blocking(SOCKET socket) {
    u_long on = 1;
    return ioctlsocket(socket, FIONBIO, &on) == 0 ? 0 : WSAGetLastError();
}

int ms_listen_loopback(uint16_t port, ms_socket *out) {
    int error = start_winsock();
    if (error) return error;
    SOCKET listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (listener == INVALID_SOCKET) return WSAGetLastError();
    // Another program must not take over the port while this one listens.
    BOOL exclusive = TRUE;
    setsockopt(listener, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (const char *)&exclusive, sizeof exclusive);
    struct sockaddr_in address = {0};
    address.sin_family = AF_INET;
    address.sin_port = htons(port);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(listener, (const struct sockaddr *)&address, sizeof address) != 0 || listen(listener, 8) != 0
        || (error = non_blocking(listener)) != 0) {
        if (!error) error = WSAGetLastError();
        closesocket(listener);
        return error;
    }
    *out = (ms_socket)listener;
    return 0;
}

int ms_accept(ms_socket listener, ms_socket *out) {
    SOCKET connection = accept((SOCKET)listener, NULL, NULL);
    if (connection == INVALID_SOCKET) {
        int error = WSAGetLastError();
        return error == WSAEWOULDBLOCK ? 1 : error;
    }
    // Accepted sockets inherit non-blocking mode; set it anyway.
    int error = non_blocking(connection);
    if (error) {
        closesocket(connection);
        return error;
    }
    *out = (ms_socket)connection;
    return 0;
}

int ms_receive(ms_socket socket, uint8_t *buffer, size_t capacity, size_t *received) {
    int count = recv((SOCKET)socket, (char *)buffer, capacity > INT_MAX ? INT_MAX : (int)capacity, 0);
    if (count == SOCKET_ERROR) {
        int error = WSAGetLastError();
        return error == WSAEWOULDBLOCK ? 1 : error;
    }
    *received = (size_t)count;
    return 0;
}

int ms_send(ms_socket socket, const uint8_t *data, size_t length, size_t *sent) {
    int count = send((SOCKET)socket, (const char *)data, length > INT_MAX ? INT_MAX : (int)length, 0);
    if (count == SOCKET_ERROR) {
        int error = WSAGetLastError();
        return error == WSAEWOULDBLOCK ? 1 : error;
    }
    *sent = (size_t)count;
    return 0;
}

void ms_close_socket(ms_socket socket) {
    shutdown((SOCKET)socket, SD_BOTH);
    closesocket((SOCKET)socket);
}

// MARK: - DNS

struct ms_dns_query {
    volatile LONG references;
    volatile LONG delivered;
    ms_dns_callback callback;
    void *context;
    wchar_t *name;
    DNS_QUERY_RESULT result;
    DNS_QUERY_CANCEL cancel;
};

static void release_query(ms_dns_query *query) {
    if (InterlockedDecrement(&query->references) == 0) {
        free(query->name);
        free(query);
    }
}

/// Hands the TXT answers in `records` to the callback, once, and frees them.
static void deliver(ms_dns_query *query, DNS_STATUS status, PDNS_RECORD records) {
    if (InterlockedExchange(&query->delivered, 1) != 0) {
        if (records) DnsRecordListFree(records, DnsFreeRecordList);
        return;
    }
    size_t capacity = 0;
    for (PDNS_RECORD record = records; status == 0 && record; record = record->pNext) capacity++;
    char **texts = capacity ? calloc(capacity, sizeof(char *)) : NULL;
    size_t count = 0;
    for (PDNS_RECORD record = records; texts && record; record = record->pNext) {
        if (record->wType != DNS_TYPE_TEXT || record->Flags.S.Section != DnsSectionAnswer) continue;
        // One record's character-strings, concatenated.
        size_t total = 0;
        for (DWORD i = 0; i < record->Data.TXT.dwStringCount; i++) total += wcslen(record->Data.TXT.pStringArray[i]);
        wchar_t *joined = calloc(total + 1, sizeof(wchar_t));
        if (!joined) continue;
        for (DWORD i = 0; i < record->Data.TXT.dwStringCount; i++) wcscat_s(joined, total + 1, record->Data.TXT.pStringArray[i]);
        texts[count] = narrow(joined, -1, NULL);
        free(joined);
        if (texts[count]) count++;
    }
    if (records) DnsRecordListFree(records, DnsFreeRecordList);
    query->callback(query->context, (const char *const *)texts, count);
    for (size_t i = 0; i < count; i++) free(texts[i]);
    free(texts);
}

static VOID WINAPI query_completed(PVOID context, PDNS_QUERY_RESULT result) {
    ms_dns_query *query = context;
    deliver(query, result->QueryStatus, result->pQueryRecords);
    release_query(query);
}

ms_dns_query *ms_dns_txt_start(const char *name, ms_dns_callback callback, void *context) {
    ms_dns_query *query = calloc(1, sizeof *query);
    if (!query) return NULL;
    query->name = wide(name);
    if (!query->name) {
        free(query);
        return NULL;
    }
    // One reference for the caller, one for the completion.
    query->references = 2;
    query->callback = callback;
    query->context = context;
    query->result.Version = DNS_QUERY_RESULTS_VERSION1;
    DNS_QUERY_REQUEST request = {0};
    request.Version = DNS_QUERY_REQUEST_VERSION1;
    request.QueryName = query->name;
    request.QueryType = DNS_TYPE_TEXT;
    request.QueryOptions = DNS_QUERY_STANDARD;
    request.pQueryCompletionCallback = query_completed;
    request.pQueryContext = query;
    DNS_STATUS status = DnsQueryEx(&request, &query->result, &query->cancel);
    if (status != DNS_REQUEST_PENDING) {
        // Answered (or failed) at once: the completion will not be called.
        deliver(query, status, query->result.pQueryRecords);
        release_query(query);
    }
    return query;
}

void ms_dns_txt_cancel(ms_dns_query *query) {
    if (query->delivered) return;
    // Fails harmlessly when the query has completed meanwhile; otherwise the completion runs with
    // ERROR_CANCELLED.
    DnsCancelQuery(&query->cancel);
}

void ms_dns_txt_release(ms_dns_query *query) { release_query(query); }

// MARK: - Images

/// The frame of `decoder` with the most pixels (an icon file holds several sizes).
static HRESULT largest_frame(IWICBitmapDecoder *decoder, IWICBitmapFrameDecode **out) {
    UINT frames = 0;
    HRESULT hr = IWICBitmapDecoder_GetFrameCount(decoder, &frames);
    if (FAILED(hr)) return hr;
    if (frames == 0) return WINCODEC_ERR_FRAMEMISSING;
    *out = NULL;
    UINT64 best = 0;
    for (UINT i = 0; i < frames; i++) {
        IWICBitmapFrameDecode *frame = NULL;
        UINT width = 0, height = 0;
        if (FAILED(IWICBitmapDecoder_GetFrame(decoder, i, &frame))) continue;
        if (SUCCEEDED(IWICBitmapFrameDecode_GetSize(frame, &width, &height)) && (UINT64)width * height > best) {
            best = (UINT64)width * height;
            if (*out) IWICBitmapFrameDecode_Release(*out);
            *out = frame;
        } else {
            IWICBitmapFrameDecode_Release(frame);
        }
    }
    return *out ? S_OK : WINCODEC_ERR_FRAMEMISSING;
}

/// `source` scaled to `width`×`height`, the `rect` of it copied into `pixels` (premultiplied BGRA).
static HRESULT scaled_pixels(IWICImagingFactory *factory, IWICBitmapSource *source, UINT width, UINT height,
                             const WICRect *rect, BYTE *pixels) {
    IWICBitmapScaler *scaler = NULL;
    HRESULT hr = IWICImagingFactory_CreateBitmapScaler(factory, &scaler);
    if (SUCCEEDED(hr)) hr = IWICBitmapScaler_Initialize(scaler, source, width, height, WICBitmapInterpolationModeHighQualityCubic);
    if (SUCCEEDED(hr)) {
        UINT stride = (UINT)rect->Width * 4;
        hr = IWICBitmapScaler_CopyPixels(scaler, rect, stride, stride * (UINT)rect->Height, pixels);
    }
    if (scaler) IWICBitmapScaler_Release(scaler);
    return hr;
}

/// Premultiplied BGRA `pixels` as a PNG.
static HRESULT encode_png(IWICImagingFactory *factory, BYTE *pixels, UINT side, uint8_t **png, size_t *png_length) {
    // PNG stores straight alpha.
    for (UINT i = 0; i < side * side; i++) {
        BYTE *pixel = pixels + i * 4;
        BYTE alpha = pixel[3];
        if (alpha == 0 || alpha == 255) continue;
        for (int c = 0; c < 3; c++) pixel[c] = (BYTE)((pixel[c] * 255 + alpha / 2) / alpha);
    }
    IStream *stream = NULL;
    IWICBitmapEncoder *encoder = NULL;
    IWICBitmapFrameEncode *frame = NULL;
    IPropertyBag2 *options = NULL;
    HRESULT hr = CreateStreamOnHGlobal(NULL, TRUE, &stream);
    if (SUCCEEDED(hr)) hr = IWICImagingFactory_CreateEncoder(factory, &GUID_ContainerFormatPng, NULL, &encoder);
    if (SUCCEEDED(hr)) hr = IWICBitmapEncoder_Initialize(encoder, stream, WICBitmapEncoderNoCache);
    if (SUCCEEDED(hr)) hr = IWICBitmapEncoder_CreateNewFrame(encoder, &frame, &options);
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameEncode_Initialize(frame, options);
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameEncode_SetSize(frame, side, side);
    WICPixelFormatGUID format = GUID_WICPixelFormat32bppBGRA;
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameEncode_SetPixelFormat(frame, &format);
    if (SUCCEEDED(hr) && !IsEqualGUID(&format, &GUID_WICPixelFormat32bppBGRA)) hr = WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT;
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameEncode_WritePixels(frame, side, side * 4, side * side * 4, pixels);
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameEncode_Commit(frame);
    if (SUCCEEDED(hr)) hr = IWICBitmapEncoder_Commit(encoder);
    HGLOBAL memory = NULL;
    STATSTG stat;
    if (SUCCEEDED(hr)) hr = GetHGlobalFromStream(stream, &memory);
    if (SUCCEEDED(hr)) hr = IStream_Stat(stream, &stat, STATFLAG_NONAME);
    if (SUCCEEDED(hr)) {
        size_t length = (size_t)stat.cbSize.QuadPart;
        void *bytes = GlobalLock(memory);
        *png = malloc(length ? length : 1);
        if (bytes && *png) {
            memcpy(*png, bytes, length);
            *png_length = length;
        } else {
            free(*png);
            *png = NULL;
            hr = E_OUTOFMEMORY;
        }
        if (bytes) GlobalUnlock(memory);
    }
    if (options) IPropertyBag2_Release(options);
    if (frame) IWICBitmapFrameEncode_Release(frame);
    if (encoder) IWICBitmapEncoder_Release(encoder);
    if (stream) IStream_Release(stream);
    return hr;
}

static HRESULT draw(IWICImagingFactory *factory, const uint8_t *data, size_t length, int mode, UINT side,
                    uint8_t **png, size_t *png_length, int *source_pixels, int *corners_clear) {
    if (length > MAXDWORD || side == 0 || side > 4096) return E_INVALIDARG;
    IWICStream *stream = NULL;
    IWICBitmapDecoder *decoder = NULL;
    IWICBitmapFrameDecode *frame = NULL;
    IWICFormatConverter *converter = NULL;
    BYTE *canvas = NULL, *scaled = NULL;
    UINT width = 0, height = 0;
    HRESULT hr = IWICImagingFactory_CreateStream(factory, &stream);
    if (SUCCEEDED(hr)) hr = IWICStream_InitializeFromMemory(stream, (BYTE *)data, (DWORD)length);
    if (SUCCEEDED(hr)) hr = IWICImagingFactory_CreateDecoderFromStream(factory, (IStream *)stream, NULL,
                                                                       WICDecodeMetadataCacheOnDemand, &decoder);
    if (SUCCEEDED(hr)) hr = largest_frame(decoder, &frame);
    if (SUCCEEDED(hr)) hr = IWICBitmapFrameDecode_GetSize(frame, &width, &height);
    if (SUCCEEDED(hr) && (width == 0 || height == 0)) hr = WINCODEC_ERR_BADIMAGE;
    if (SUCCEEDED(hr)) hr = IWICImagingFactory_CreateFormatConverter(factory, &converter);
    if (SUCCEEDED(hr)) hr = IWICFormatConverter_Initialize(converter, (IWICBitmapSource *)frame, &GUID_WICPixelFormat32bppPBGRA,
                                                           WICBitmapDitherTypeNone, NULL, 0.0, WICBitmapPaletteTypeCustom);
    if (SUCCEEDED(hr)) {
        *source_pixels = (int)(width > height ? width : height);
        canvas = calloc((size_t)side * side, 4);
        if (!canvas) hr = E_OUTOFMEMORY;
    }
    if (SUCCEEDED(hr) && mode == MS_IMAGE_FILL) {
        // Covers the canvas; only the centered square of the scaled image is computed.
        double scale = (double)side / width > (double)side / height ? (double)side / width : (double)side / height;
        UINT drawnWidth = (UINT)(width * scale + 0.5), drawnHeight = (UINT)(height * scale + 0.5);
        if (drawnWidth < side) drawnWidth = side;
        if (drawnHeight < side) drawnHeight = side;
        WICRect rect = {(INT)((drawnWidth - side) / 2), (INT)((drawnHeight - side) / 2), (INT)side, (INT)side};
        hr = scaled_pixels(factory, (IWICBitmapSource *)converter, drawnWidth, drawnHeight, &rect, canvas);
    } else if (SUCCEEDED(hr)) {
        // On white, inside an 18% margin.
        memset(canvas, 255, (size_t)side * side * 4);
        double box = side - 2 * (side * 0.18);
        double scale = box / width < box / height ? box / width : box / height;
        UINT drawnWidth = (UINT)(width * scale + 0.5), drawnHeight = (UINT)(height * scale + 0.5);
        if (drawnWidth == 0) drawnWidth = 1;
        if (drawnHeight == 0) drawnHeight = 1;
        scaled = malloc((size_t)drawnWidth * drawnHeight * 4);
        if (!scaled) hr = E_OUTOFMEMORY;
        WICRect rect = {0, 0, (INT)drawnWidth, (INT)drawnHeight};
        if (SUCCEEDED(hr)) hr = scaled_pixels(factory, (IWICBitmapSource *)converter, drawnWidth, drawnHeight, &rect, scaled);
        if (SUCCEEDED(hr)) {
            UINT left = (side - drawnWidth) / 2, top = (side - drawnHeight) / 2;
            for (UINT y = 0; y < drawnHeight; y++) {
                for (UINT x = 0; x < drawnWidth; x++) {
                    const BYTE *source = scaled + ((size_t)y * drawnWidth + x) * 4;
                    BYTE *target = canvas + ((size_t)(top + y) * side + left + x) * 4;
                    // Source over, premultiplied.
                    for (int c = 0; c < 4; c++) target[c] = (BYTE)(source[c] + target[c] * (255 - source[3]) / 255);
                }
            }
        }
    }
    if (SUCCEEDED(hr)) {
        UINT edge = side > 2 ? side - 2 : 0;
        UINT corners[4][2] = {{1, 1}, {edge, 1}, {1, edge}, {edge, edge}};
        *corners_clear = 0;
        for (int i = 0; i < 4; i++) {
            if (canvas[((size_t)corners[i][1] * side + corners[i][0]) * 4 + 3] < 128) *corners_clear = 1;
        }
        hr = encode_png(factory, canvas, side, png, png_length);
    }
    free(scaled);
    free(canvas);
    if (converter) IWICFormatConverter_Release(converter);
    if (frame) IWICBitmapFrameDecode_Release(frame);
    if (decoder) IWICBitmapDecoder_Release(decoder);
    if (stream) IWICStream_Release(stream);
    return hr;
}

int ms_image_draw(const uint8_t *data, size_t length, int mode, int side,
                  uint8_t **png, size_t *png_length, int *source_pixels, int *corners_clear) {
    HRESULT hr;
    BOOL entered = enter_com(&hr);
    if (FAILED(hr)) return (int)hr;
    IWICImagingFactory *factory = NULL;
    hr = CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&factory);
    if (SUCCEEDED(hr)) {
        hr = draw(factory, data, length, mode, side < 0 ? 0 : (UINT)side, png, png_length, source_pixels, corners_clear);
        IWICImagingFactory_Release(factory);
    }
    if (entered) CoUninitialize();
    return SUCCEEDED(hr) ? 0 : (int)hr;
}

// MARK: - File types and charsets

int ms_mime_type(const char *extension, char *buffer, size_t capacity) {
    size_t length = strlen(extension);
    char *dotted = malloc(length + 2);
    if (!dotted) return ERROR_OUTOFMEMORY;
    dotted[0] = '.';
    memcpy(dotted + 1, extension, length + 1);
    wchar_t *key = wide(dotted);
    free(dotted);
    if (!key) return ERROR_INVALID_PARAMETER;
    wchar_t value[256];
    DWORD size = sizeof value;
    LSTATUS status = RegGetValueW(HKEY_CLASSES_ROOT, key, L"Content Type", RRF_RT_REG_SZ, NULL, value, &size);
    free(key);
    if (status != ERROR_SUCCESS) return (int)status;
    size_t written = 0;
    char *text = narrow(value, -1, &written);
    if (!text) return ERROR_INVALID_DATA;
    int result = ERROR_INSUFFICIENT_BUFFER;
    if (written > 0 && written < capacity) {
        memcpy(buffer, text, written + 1);
        result = 0;
    }
    free(text);
    return result;
}

int ms_decode_charset(const char *charset, const uint8_t *data, size_t length, char **utf8, size_t *utf8_length) {
    if (length > INT32_MAX) return U_ILLEGAL_ARGUMENT_ERROR;
    UErrorCode status = U_ZERO_ERROR;
    UConverter *converter = ucnv_open(charset, &status);
    if (U_FAILURE(status)) return status;
    // Bytes not valid in the charset fail the conversion rather than turn into replacements.
    ucnv_setToUCallBack(converter, UCNV_TO_U_CALLBACK_STOP, NULL, NULL, NULL, &status);
    int32_t units = U_SUCCESS(status) ? ucnv_toUChars(converter, NULL, 0, (const char *)data, (int32_t)length, &status) : 0;
    if (status == U_BUFFER_OVERFLOW_ERROR || status == U_STRING_NOT_TERMINATED_WARNING) status = U_ZERO_ERROR;
    UChar *text = NULL;
    if (U_SUCCESS(status)) {
        text = malloc(sizeof(UChar) * ((size_t)units + 1));
        if (!text) status = U_MEMORY_ALLOCATION_ERROR;
    }
    if (U_SUCCESS(status)) {
        ucnv_reset(converter);
        ucnv_toUChars(converter, text, units + 1, (const char *)data, (int32_t)length, &status);
    }
    ucnv_close(converter);
    if (U_SUCCESS(status)) {
        *utf8 = narrow((const wchar_t *)text, units, utf8_length);
        if (!*utf8) status = U_INVALID_CHAR_FOUND;
    }
    free(text);
    return U_SUCCESS(status) ? 0 : (int)status;
}

// MARK: - Directory changes

void *ms_watch_directory(const char *directory) {
    wchar_t *path = wide(directory);
    if (!path) return NULL;
    HANDLE handle = FindFirstChangeNotificationW(path, FALSE, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE
                                                 | FILE_NOTIFY_CHANGE_SIZE);
    free(path);
    return handle == INVALID_HANDLE_VALUE ? NULL : handle;
}

int ms_watch_changed(void *watch) {
    if (WaitForSingleObject(watch, 0) != WAIT_OBJECT_0) return 0;
    FindNextChangeNotification(watch);
    return 1;
}

void ms_watch_close(void *watch) { FindCloseChangeNotification(watch); }
