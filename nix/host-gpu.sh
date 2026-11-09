# Expose only host NVIDIA driver libraries, not the host libc, to Nix programs.
if [ -f /usr/share/vulkan/icd.d/nvidia_icd.json ]; then
    driver_links="${XDG_CACHE_HOME:-$HOME/.cache}/chr/host-nvidia"
    mkdir -p -m 700 "$driver_links"
    for library in \
        /usr/lib/x86_64-linux-gnu/libnvidia-*.so* \
        /usr/lib/x86_64-linux-gnu/libGLX_nvidia.so* \
        /usr/lib/x86_64-linux-gnu/libEGL_nvidia.so* \
        /usr/lib/x86_64-linux-gnu/libcuda.so* \
        /usr/lib/x86_64-linux-gnu/libnvcuvid.so*; do
        if [ -f "$library" ]; then
            ln -sfn "$library" "$driver_links/$(basename "$library")"
        fi
    done
    export LD_LIBRARY_PATH="$driver_links${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export VK_DRIVER_FILES="${VK_DRIVER_FILES:-/usr/share/vulkan/icd.d/nvidia_icd.json}"
    if [ -f /usr/share/glvnd/egl_vendor.d/10_nvidia.json ]; then
        export __EGL_VENDOR_LIBRARY_FILENAMES="${__EGL_VENDOR_LIBRARY_FILENAMES:-/usr/share/glvnd/egl_vendor.d/10_nvidia.json}"
    fi
fi
