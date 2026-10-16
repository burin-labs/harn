Rust CI cache downloads abandon a stalled segment after two minutes and fall back to compiling from source.
The timeout applies to both the shared cache and its broad fallback, while all build and test checks remain required.
