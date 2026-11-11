cmake_minimum_required(VERSION 3.13)
project(runner LANGUAGES C CXX)

# The runner is this folder's Rust crate (src/lib.rs), a static library that
# defines `main`. Cargo builds it, with bindings of Flutter's embedder headers
# (build.rs); CMake links it into the executable with Flutter, GTK and the
# plugins. Nothing here is C++ but the registrant Flutter generates.
find_program(CARGO cargo REQUIRED)
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(RUNNER_PROFILE "debug")
  set(RUNNER_CARGO_FLAGS "")
else()
  set(RUNNER_PROFILE "release")
  set(RUNNER_CARGO_FLAGS "--release")
endif()
set(RUNNER_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/cargo")
set(RUNNER_LIBRARY "${RUNNER_TARGET_DIR}/${RUNNER_PROFILE}/libmailstream_runner.a")
add_custom_target(runner_crate
  COMMAND ${CMAKE_COMMAND} -E env
    "APPLICATION_ID=${APPLICATION_ID}"
    "FLUTTER_MANAGED_DIR=${FLUTTER_MANAGED_DIR}"
    ${CARGO} build ${RUNNER_CARGO_FLAGS}
    --manifest-path "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
    --target-dir "${RUNNER_TARGET_DIR}"
  BYPRODUCTS "${RUNNER_LIBRARY}"
  USES_TERMINAL
)
# The embedder's headers the bindings are made from come with the assembly.
add_dependencies(runner_crate flutter_assemble)

# The registrant (C in all but its name) is compiled as C, so the crate calls
# fl_register_plugins by its plain name. CMP0119 makes the compiler read it as
# C (`-x c`) rather than by its extension.
cmake_policy(SET CMP0119 NEW)
set(REGISTRANT "${FLUTTER_MANAGED_DIR}/generated_plugin_registrant.cc")
set_source_files_properties("${REGISTRANT}" PROPERTIES LANGUAGE C)

# Define the application target. To change its name, change BINARY_NAME in the
# top-level CMakeLists.txt, not the value here, or `flutter run` will no longer
# work.
add_executable(${BINARY_NAME} "${REGISTRANT}")
add_dependencies(${BINARY_NAME} runner_crate)

# Apply the standard set of build settings. This can be removed for applications
# that need different build settings.
apply_standard_settings(${BINARY_NAME})

# The crate, then what it calls: Flutter's embedder, GTK, and the C and system
# libraries Rust's standard library needs.
target_link_libraries(${BINARY_NAME} PRIVATE "${RUNNER_LIBRARY}")
target_link_libraries(${BINARY_NAME} PRIVATE flutter)
target_link_libraries(${BINARY_NAME} PRIVATE PkgConfig::GTK)
target_link_libraries(${BINARY_NAME} PRIVATE ${CMAKE_DL_LIBS} pthread m gcc_s)

target_include_directories(${BINARY_NAME} PRIVATE "${CMAKE_SOURCE_DIR}")
