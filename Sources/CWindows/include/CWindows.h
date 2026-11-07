// The Windows system calls the engine makes where the Mac uses Apple frameworks (Security,
// Network, NSWorkspace, AppKit images, dnssd, UniformTypeIdentifiers, CoreFoundation charsets,
// vnode watches). Only Package.swift's Windows branch builds this target.
//
// Strings in and out are UTF-8. Memory a function hands out is freed with ms_free.
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void ms_free(void *pointer);

// MARK: - Data protection (DPAPI, bound to the signed-in Windows user)

/// 0 on success, else the system error.
int ms_protect(const uint8_t *data, size_t length, uint8_t **out, size_t *out_length);
int ms_unprotect(const uint8_t *data, size_t length, uint8_t **out, size_t *out_length);

// MARK: - Shell

/// Opens `url` with its default handler (the browser for https). 0 on success.
int ms_open_url(const char *url);

// MARK: - Loopback TCP (non-blocking sockets)

typedef uintptr_t ms_socket;

/// Listens on 127.0.0.1:`port`. 0 on success, else the WinSock error.
int ms_listen_loopback(uint16_t port, ms_socket *out);
/// 0: `out` is a new connection; 1: none waiting; else the WinSock error.
int ms_accept(ms_socket listener, ms_socket *out);
/// 0: `received` bytes read (0 at the end of the stream); 1: nothing yet; else the WinSock error.
int ms_receive(ms_socket socket, uint8_t *buffer, size_t capacity, size_t *received);
/// 0: `sent` bytes written; 1: the buffer is full, try later; else the WinSock error.
int ms_send(ms_socket socket, const uint8_t *data, size_t length, size_t *sent);
void ms_close_socket(ms_socket socket);

// MARK: - DNS TXT records (DnsQueryEx, asynchronous)

typedef struct ms_dns_query ms_dns_query;
/// Each record's character-strings concatenated; `records` is valid only during the call.
typedef void (*ms_dns_callback)(void *context, const char *const *records, size_t count);

/// Starts a TXT query for `name`. Unless it returns NULL, `callback` runs exactly once, on a
/// system thread or before this returns, with the records (none on failure or cancellation).
/// The caller releases the query with ms_dns_txt_release once it no longer cancels it.
ms_dns_query *ms_dns_txt_start(const char *name, ms_dns_callback callback, void *context);
/// Ends a query still running; its callback then gets no records.
void ms_dns_txt_cancel(ms_dns_query *query);
void ms_dns_txt_release(ms_dns_query *query);

// MARK: - Images (Windows Imaging Component)

enum { MS_IMAGE_FILL = 0, MS_IMAGE_INSET = 1 };

/// Decodes `data` (any format WIC reads; for icons the largest frame) and draws it on a
/// `side`×`side` canvas: MS_IMAGE_FILL scales it to cover the canvas, centered; MS_IMAGE_INSET
/// fits it inside an 18% margin on white. `png` gets the canvas as PNG, `source_pixels` the
/// larger dimension of the decoded image, `corners_clear` whether any of the canvas's four corner
/// pixels is mostly transparent. 0 on success, else the HRESULT.
int ms_image_draw(const uint8_t *data, size_t length, int mode, int side,
                  uint8_t **png, size_t *png_length, int *source_pixels, int *corners_clear);

// MARK: - File types and charsets

/// The MIME type Windows registers for file extension `extension` (without the dot), copied into
/// `buffer`. 0 when there is one.
int ms_mime_type(const char *extension, char *buffer, size_t capacity);

/// `data` in `charset` (an IANA name) as UTF-8, through the system's ICU. 0 on success; failure
/// when the charset is unknown or the bytes are not valid in it.
int ms_decode_charset(const char *charset, const uint8_t *data, size_t length,
                      char **utf8, size_t *utf8_length);

// MARK: - Directory changes

/// Watches `directory` (not its subdirectories) for files created, deleted, renamed or written.
/// NULL when it cannot.
void *ms_watch_directory(const char *directory);
/// 1 when something changed since the last call (or since the watch began), else 0. Never blocks.
int ms_watch_changed(void *watch);
void ms_watch_close(void *watch);

#ifdef __cplusplus
}
#endif
