# TODO

## Windows sender latency push

10ms buffer operation is already close to the practical scheduling limit for a
normal Windows/macOS user-space audio pipeline. Further latency work should be
done primarily on the Windows sender side, and should stay measurement-first so
we do not jump into ASIO complexity without evidence.

1. [x] Make the Windows sender thread use MMCSS / high priority.
   - The sender packet dispatch / pacing loop now enters MMCSS `Pro Audio`
     on Windows, with normal high thread priority as a fallback.
   - CPAL is built with its `realtime` feature for the sender so the WASAPI
     capture callback thread can use CPAL's realtime promotion path too.
   - Network I/O remains outside the capture callback.

2. [x] Split sender-side timing metrics.
   - Added `capture_callback_gap_max` to show capture callback clustering.
   - Added `packet_dispatch_gap_max` to show packet pacing / send clustering.
   - Both are visible in the sender log so receiver-side 10ms spikes can be
     matched against sender-side causes.

3. [x] Check whether CPAL can provide a WASAPI exclusive/event-mode equivalent.
   - CPAL 0.18.1's WASAPI backend uses shared-mode event callbacks internally.
   - CPAL does not expose a public shared/exclusive switch through
     `StreamConfig`; the available low-complexity knob is
     `BufferSize::Fixed`.
   - Added `--input-buffer-size-frames` and Windows launcher plumbing so small
     CPAL input buffers can be tested before adding ASIO-specific code.

4. [x] Add native event-driven `w-sender` for Windows / VB-CABLE.
   - `w-sender` uses native WASAPI shared event capture and sends UDP packets
     immediately from each capture event.
   - It does not use sender-side packet pacing, capture backlog queues, or
     receiver feedback ASRC.
   - Localhost self-tests are recorded in `wSELFTEST.md`; on this machine,
     VB-CABLE delivered 480-frame events at about 100 events/s, and
     `event_to_send_max` stayed below 1ms in the observed runs.

5. [ ] Consider ASIO only if `w-sender` / WASAPI event metrics still show
   capture-event gaps that are too coarse for the target use case.
   - Current VB-CABLE event cadence on the tested machine is roughly 10ms
     (`event_gap_max` around 11-12ms), which is treated as Windows/VB-CABLE
     fixed external latency rather than sender packet pacing latency.
   - ASIO may help if the remaining spikes come from WASAPI / virtual cable
     capture scheduling.
   - It is likely to add significant Windows-specific complexity, so require
     evidence before taking this path.
