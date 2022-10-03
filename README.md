# blocked-vec

A blocked vector, used like a [`File`](https://doc.rust-lang.org/std/fs/struct.File.html) 
but is implemented purely in memory, supporting all vectorized operations. The APIs are 
pretty similar, like general `std::io` traits implementation, `set_len` (called `resize` 
here), `append`, and `XX_at` operations (like 
[`FileExt`](https://doc.rust-lang.org/std/os/unix/fs/trait.FileExt.html) on *nix platforms).

## Implementaton

This structure is implemented as a vector of blocks, as the name implies. A block consists
of an array of continuous memory pages, whose layout is either through
[querying the system](https://docs.rs/page_size/latest/page_size/fn.get.html) or from the
given parameter (`new_paged`, `with_len_paged`). This implementation is used to avoid 
frequent calls of reallocation methods when manipulating with massive data.
