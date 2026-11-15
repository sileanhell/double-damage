// Minimal MSVC C++ runtime pieces required by WebView2LoaderStatic.lib (built with MSVC),
// so it can be linked statically on the windows-gnullvm toolchain.
#include <stdint.h>
#include <stdlib.h>
#include <windows.h>

// ---- /GS stack cookie --------------------------------------------------------
uintptr_t __security_cookie = 0x00002B992DDFA232ull;

void __security_check_cookie(uintptr_t cookie) {
    if (cookie != __security_cookie) {
        __fastfail(2 /* FAST_FAIL_STACK_COOKIE_CHECK_FAILURE */);
    }
}

// ---- operator new / delete (MSVC-mangled names) ------------------------------
typedef struct { char unused; } nothrow_t;
const nothrow_t msvc_nothrow __asm__("?nothrow@std@@3Unothrow_t@1@B") = {0};

static void *checked_alloc(size_t size) {
    void *p = malloc(size ? size : 1);
    if (!p) abort();
    return p;
}

void *msvc_new_array(size_t size) __asm__("??_U@YAPEAX_K@Z");
void *msvc_new_array(size_t size) { return checked_alloc(size); }

void *msvc_new_nothrow(size_t size, const nothrow_t *tag) __asm__("??2@YAPEAX_KAEBUnothrow_t@std@@@Z");
void *msvc_new_nothrow(size_t size, const nothrow_t *tag) { (void)tag; return malloc(size ? size : 1); }

void msvc_delete_array(void *p) __asm__("??_V@YAXPEAX@Z");
void msvc_delete_array(void *p) { free(p); }

void msvc_delete_sized(void *p, size_t size) __asm__("??3@YAXPEAX_K@Z");
void msvc_delete_sized(void *p, size_t size) { (void)size; free(p); }

// ---- thread-safe local statics (same protocol as vcruntime thread_safe_statics.cpp)
#define EPOCH_START INT32_MIN
#define UNINITIALIZED 0
#define BEING_INITIALIZED (-1)

int _Init_global_epoch = EPOCH_START;
__thread int _Init_thread_epoch = EPOCH_START;

static SRWLOCK init_lock = SRWLOCK_INIT;
static CONDITION_VARIABLE init_cv = CONDITION_VARIABLE_INIT;

void _Init_thread_header(int *once) {
    AcquireSRWLockExclusive(&init_lock);
    if (*once == UNINITIALIZED) {
        *once = BEING_INITIALIZED;
    } else {
        while (*once == BEING_INITIALIZED) {
            SleepConditionVariableSRW(&init_cv, &init_lock, INFINITE, 0);
            if (*once == UNINITIALIZED) {
                *once = BEING_INITIALIZED;
                break;
            }
        }
        _Init_thread_epoch = _Init_global_epoch;
    }
    ReleaseSRWLockExclusive(&init_lock);
}

void _Init_thread_footer(int *once) {
    AcquireSRWLockExclusive(&init_lock);
    ++_Init_global_epoch;
    *once = _Init_global_epoch;
    _Init_thread_epoch = _Init_global_epoch;
    ReleaseSRWLockExclusive(&init_lock);
    WakeAllConditionVariable(&init_cv);
}

void _Init_thread_abort(int *once) {
    AcquireSRWLockExclusive(&init_lock);
    *once = UNINITIALIZED;
    ReleaseSRWLockExclusive(&init_lock);
    WakeAllConditionVariable(&init_cv);
}
