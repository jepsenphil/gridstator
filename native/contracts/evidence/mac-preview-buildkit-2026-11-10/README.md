# Historical Mac BuildKit preview proof

These are actual final-byte foreground controller/worker and uniquely labelled user LaunchAgent checks of `native-preview-20261110-1fd6e3d425b3` on macOS 15.6. ARM64 ran natively on an M4; amd64 ran under Rosetta, not on physical Intel hardware. Both targets passed the targeted local-image metadata test, strict all-target Clippy and two real foreground/user-service cases. The exact source/build inputs and binary identity are recorded in the checkpoint and build-info files.

These role cases intentionally selected unavailable Docker/act binaries to verify native-role operation without external tools. Actual Docker execution was proved separately with the temporary corrected-memory overlay in `../m4-memory-docker-2026-11-10`; this directory does not claim that later source behavior for the frozen 1fd bytes. ARM role records also retain the original zero host-available sample before the sampler correction.

These are unsigned development preview executables: no Developer ID signing or notarization claim, no stable release publication. Private user labels were removed by the tests and independently confirmed absent; retained private role credentials/state were removed after collecting these public records. The immutable historical payload files remain unchanged.
