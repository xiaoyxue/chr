# Repository instructions

- Use English for all repository content, including code, comments, and documentation.
- Manage all project dependencies with Nix. Do not install dependencies with pip or uv.
- Build the Dawn/C++ backend with CMake and Clang.
- Keep private videos, artwork, camera data, masks, credentials, and runtime outputs outside Git.
- Preserve frame identity when associating video with camera transforms and occlusion masks.
- Decoded and rendered video pixels must remain on the GPU. Do not introduce
  rawvideo CPU pipes, frame downloads, or CPU compositing as a fallback.
- Distinguish GPU-local copies from zero-copy resource sharing; describe both accurately.
