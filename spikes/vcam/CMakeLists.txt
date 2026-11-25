cmake_minimum_required(VERSION 3.20)
cmake_policy(SET CMP0091 NEW)

project(camdesk_vcam_spike LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

add_library(cellcam_spike SHARED
    dllmain.cpp
    media.cpp
    shm.cpp
    cellcam_spike.def)
add_executable(vcam_spike
    host.cpp
    shm.cpp)

foreach(target cellcam_spike vcam_spike)
    target_compile_definitions(${target} PRIVATE
        UNICODE
        _UNICODE
        NTDDI_VERSION=0x0A000010
        _WIN32_WINNT=0x0A00
        WINVER=0x0A00)
    target_link_libraries(${target} PRIVATE
        mfplat
        mfuuid
        mf
        ole32
        advapi32
        shell32)
endforeach()

target_link_libraries(vcam_spike PRIVATE mfreadwrite mfsensorgroup)
