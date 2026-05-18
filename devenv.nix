{ pkgs, lib, config, inputs, ... }:
{
    packages = with pkgs; [
        boxes
        cargo-llvm-cov
        glibcLocales
        jjui
        jujutsu
        just
        llvm
    ];

    env.LOCALE_ARCHIVE = "${pkgs.glibcLocales}/lib/locale/locale-archive";

    languages.rust = {
        enable = true;
        toolchainFile = ./rust-toolchain.toml;
    };

    scripts.versions.exec = ''
        cat <<-EOF | boxes -d info
        	Nix:    $(nix --version | cut -f3 -d' ')
        	Devenv: $(devenv version | cut -f2 -d' ')
        	Direnv: $(direnv --version)

        	Rust:    $(rustc --version | cut -f2 -d' ')
        	Cargo:   $(cargo --version | cut -f2 -d' ')
        	Clippy:  $(cargo clippy --version | cut -f2 -d' ')
        	Rustfmt: $(cargo fmt --version | cut -f2 -d' ')

        	Jujutsu: $(jj --version | cut -f2 -d' ')
        EOF
    '';

    enterShell = ''
        versions
    '';
}
