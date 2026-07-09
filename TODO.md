# TODO

## Windows sender latency push

10ms buffer operation is already close to the practical scheduling limit for a
normal Windows/macOS user-space audio pipeline. Further latency work should be
done primarily on the Windows sender side, and should stay measurement-first so
we do not jump into ASIO complexity without evidence.

1. Make the Windows sender thread use MMCSS / high priority.
   - Target the packet dispatch / pacing thread first.
   - Keep network I/O out of the capture callback.

2. Split sender-side timing metrics.
   - Add `capture_callback_gap_max` to show capture callback clustering.
   - Add `packet_dispatch_gap_max` to show packet pacing / send clustering.
   - Keep these visible in the sender log so receiver-side 10ms spikes can be
     matched against sender-side causes.

3. Check whether CPAL can provide a WASAPI exclusive/event-mode equivalent.
   - Prefer a CPAL-supported path if it is available and stable enough.
   - Treat this as a lower-complexity option before adding ASIO-specific code.

4. Consider ASIO only if `capture_callback_gap_max` still shows 10ms-class
   gaps.
   - ASIO may help if the remaining spikes come from WASAPI / virtual cable
     capture scheduling.
   - It is likely to add significant Windows-specific complexity, so require
     evidence before taking this path.
