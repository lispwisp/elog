# Experimental Webserver

This is a work in progress.

Modern web servers like Hyper utilize a work stealing threadpool executor like Tokio.

These executors operate on the basis of cooperative multitasking and work stealing, usually across multiple threads.
What follows is a high level overview of how Tokio is designed:
The number of worker threads defaults to the number of logical cores of the machine.
Work items are enqueued onto buffers on each worker thread.
A worker thread that has nothing to do may steal work items in the queue of another worker thread.

Inside the worker threads, work items are expected to yield back to Tokio quickly after being polled to make progress.
For work that is blocking, Tokio contains a fairly large number of blocking threads which are dedicated for blocking tasks and which are context switched for progress by the operating system.

For yielding back to the Tokio executor, an async await abstraction is used as a signal.

Both Tokio and Hyper have many great advantages: ergonomics, legibility of async await code, and powerful abstractions.

However, there are some unfortunate pain points when using Tokio for multithreaded workloads.
For one, the work items should not be blocking or compute heavy, otherwise all the work on the queue for that worker will need to be stolen in order to make progress.
Second, it isn't trivial to know based on function signature whether or not a task is blocking or not.
Third, the illusion of infinite parallelism provided by such Executors is expensive; the illusion is supported by rapid context switching (between threads) which often flushes the cache instruction cache.
If your application involves a mixture of IO driven code with compute based code, then the processor will have to context switch between worker threads and blocking threads regularly, which may eat a bunch of cycles.


## What this is

This is an experiment to see if I can produce a more efficient webserver for high throughput use cases.

The key word here is efficiency.
The goal is to support and resolve as many concurrent connections as possible on low end hardware.

Instead of forcing the client to separate code into blocking and nonblocking work, this webserver is designed to allow intermixing async and synchronous operations.

The high level overview of how this is designed is:

A number of worker threads that defaults to the number of logical cores of the machine, just like Tokio.
One large thread local ring buffer for each worker thread.
No work stealing at all; each logical core runs through its buffer on a loop; doing compute intensive work inside this loop is fine.
Asynchronous operations like disk and network IO are arranged via an event loop (mio); the hardware responsible for those operations can work while the processor does compute on these buffers. 
Instead of context switching out of an await point, we just schedule the asynchronous work in the background with a kernel call that returns immediately, and eventually an event will be surfaced for the result.

The ring buffer is broken up into segments, typically the length of a page.
To process a request, processing occurs in stages:
1. Segment is decrypted in place.
2. Segment is decompressed in place.
3. Segment is decoded in place.
4. Segment is transmuted to the layout of the corresponding rust types in place.
5. Processing on the rust types occurs.
6. Segment is encoded in place.
7. Segment is compressed in place.
8. Segment is encrypted in place.
9. Segment is flagged to be transited out of the server.

For many of these, knowing or being able to roughly approximate the length of the resulting byte sequence before it is executed is important.
For example, base64 data encodes arbitrary 3 bytes into 4 characters which each take 1 byte.
When decoding in place, we immediately know that the last 25% of the segment will become unused.
Likewise, when encoding in place, we need to leave 25% headroom to expand into so that we do not have to shift bytes in the circular buffer.
This framework will be able to compute the appropriate segment length and will use Vectored Reads/Writes to read/write these segments, even if they are non-contiguous, with just a single syscall.

To limit the thrash of L1 cache, each segment is fully processed (if possible) before processing the next segment.
This isn't always possible, depending on the interdependencies between segments, but will be done if possible.
Because our access patterns are regular and predictable, a special `PREFETCH` machine code is used to acquire the next segment without stalling the CPU pipeline.

## Status

Work in progress; a lot remains to be implemented, am still architecting the core traits of the library and exploring the design space.
