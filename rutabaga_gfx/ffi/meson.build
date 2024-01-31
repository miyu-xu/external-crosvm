project('rutabaga-gfx-ffi', ['rust', 'c'], version: '0.1.2')

gfxstream_dep = dependency(
    'gfxstream_backend',
    fallback: ['gfxstream_backend', 'gfxstream_backend_dep'],
)

lib = gfxstream_dep.get_variable(internal: 'lib_loc', pkgconfig: 'libs-only-L')
if lib == ''
    error('Currently we rely on fallback.')
endif


shared_lib = 'librutabaga_gfx_ffi'
targetos = host_machine.system()
if targetos == 'windows'
    shared_lib += '.dll'
endif
if targetos == 'darwin'
    shared_lib +='.0.dylib'
endif
if targetos == 'linux'
    shared_lib += '.so.0'
endif


make_py = find_program('make.py')
rutabaga_gfx_ffi_h = custom_target(
    'rutabaga_gfx_ffi.h',
    output: 'include-rutabaga_gfx-rutabaga_gfx_ffi.h',
    input: ['src/include/rutabaga_gfx_ffi.h'],
    command: [make_py, '--dest', '@OUTDIR@', 'header'],
)

build_dir = meson.global_build_root()
rutabaga_gfx_ffi = custom_target(
    'rutabaga_gfx_ffi',
    output: shared_lib ,
    input: ['src/lib.rs', 'Cargo.toml', 'Cargo.lock', 'build.rs'],
    command: [make_py, '--dest', '@OUTDIR@', 'build', '--target', targetos, '--version', meson.project_version(), '--lib', lib],
    install: true,
    install_dir: get_option('libdir'),
)


rutabaga_gfx_ffi_dep = declare_dependency(
    link_with: rutabaga_gfx_ffi,
    sources: rutabaga_gfx_ffi_h,
    include_directories: include_directories('.'),
)
