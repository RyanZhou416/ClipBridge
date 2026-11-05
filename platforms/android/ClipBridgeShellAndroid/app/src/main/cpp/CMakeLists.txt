cmake_minimum_required(VERSION 3.22.1)
project(cb_jni LANGUAGES C CXX)

add_library(cb_jni SHARED cb_jni.cpp)

# 头文件：直接引用 platforms/android/include 的权威头文件，避免 cpp/include 副本与导出表漂移
# 相对路径：app/src/main/cpp → 上一级共 5 层到 platforms/android，再进 include
target_include_directories(cb_jni PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}/../../../../../include
)
target_link_directories(cb_jni PRIVATE
        ${CMAKE_SOURCE_DIR}/../jniLibs/${ANDROID_ABI}
)

find_library(log-lib log)

target_link_libraries(cb_jni
        core_ffi_android
        ${log-lib}
)
target_link_options(cb_jni PRIVATE "-Wl,-z,max-page-size=16384")
