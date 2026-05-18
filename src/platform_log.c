#include <stdarg.h>
#include <stdio.h>

// This is the function we will implement in Rust
extern void rust_log_string(const char* str);

void platformLog(const char* format, ...) {
    char buf[256]; // Adjust size based on your RAM needs
    va_list args;
    va_start(args, format);
    
    // Format the string into our buffer
    vsnprintf(buf, sizeof(buf), format, args);
    
    va_end(args);
    
    // Pass the formatted string to Rust
    rust_log_string(buf);
}