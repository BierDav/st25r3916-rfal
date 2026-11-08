/*
 * Minimal <string.h> for freestanding (bare-metal, no C library) builds of
 * the RFAL sources. Only the functions RFAL uses are declared; on Rust
 * bare-metal targets they are provided by `compiler_builtins`.
 *
 * build.rs only puts this directory on the include path for targets with
 * `target_os = "none"`.
 */
#ifndef ST25R3916_RFAL_FREESTANDING_STRING_H
#define ST25R3916_RFAL_FREESTANDING_STRING_H

#include <stddef.h>

void* memcpy( void* dest, const void* src, size_t n );
void* memmove( void* dest, const void* src, size_t n );
void* memset( void* dest, int c, size_t n );
int   memcmp( const void* a, const void* b, size_t n );

#endif /* ST25R3916_RFAL_FREESTANDING_STRING_H */
