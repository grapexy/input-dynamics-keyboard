{
  description = "Input Dynamics Keyboard Android development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { nixpkgs, ... }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      forEachSystem = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      devShells = forEachSystem (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config = {
              allowUnfree = true;
              android_sdk.accept_license = true;
            };
          };

          androidComposition = pkgs.androidenv.composeAndroidPackages {
            platformVersions = [ "35" ];
            buildToolsVersions = [ "35.0.0" ];
            abiVersions = [
              "armeabi-v7a"
              "arm64-v8a"
              "x86"
              "x86_64"
            ];
            includeNDK = true;
            ndkVersions = [ "28.0.13004108" ];
            includeEmulator = false;
            includeSystemImages = false;
            extraLicenses = [
              "android-sdk-license"
              "android-sdk-preview-license"
            ];
          };

          androidSdk = androidComposition.androidsdk;
          androidHome = "${androidSdk}/libexec/android-sdk";
          buildTools = "${androidHome}/build-tools/35.0.0";
          ndkHome = "${androidHome}/ndk/28.0.13004108";
        in
        {
          default = pkgs.mkShell {
            packages = [
              androidSdk
              pkgs.gh
              pkgs.git
              pkgs.jdk17
              pkgs.python3
              pkgs.cargo
              pkgs.cargo-deny
              pkgs.clippy
              pkgs.rust-analyzer
              pkgs.rustc
              pkgs.rustfmt
              pkgs.unzip
              pkgs.uv
              pkgs.zip
            ];

            ANDROID_HOME = androidHome;
            ANDROID_SDK_ROOT = androidHome;
            ANDROID_NDK_HOME = ndkHome;
            ANDROID_NDK_ROOT = ndkHome;
            JAVA_HOME = pkgs.jdk17.home;

            shellHook = ''
              export ANDROID_USER_HOME="$PWD/.android"
              export GH_CONFIG_DIR="$PWD/.git/gh"
              export PATH="${androidHome}/platform-tools:${buildTools}:${ndkHome}:$PATH"

              if [ -s "$GH_CONFIG_DIR/token" ]; then
                export GH_TOKEN="$(cat "$GH_CONFIG_DIR/token")"
                github_auth_status="token loaded"
              else
                github_auth_status="no local token"
              fi

              signing_env="$PWD/.git/signing/input-dynamics.env"
              if [ -s "$signing_env" ]; then
                . "$signing_env"
                signing_status="stable signing loaded"
              else
                signing_status="not loaded"
              fi

              echo "Android SDK: $ANDROID_HOME"
              echo "Android NDK: $ANDROID_NDK_HOME"
              echo "Java: $JAVA_HOME"
              echo "GitHub CLI config: $GH_CONFIG_DIR"
              echo "GitHub CLI auth: $github_auth_status"
              echo "APK signing: $signing_status"
              echo "Build with: ./gradlew :app:assembleDebugNoMinify"
              echo "Rust checks: cargo ci-fmt && cargo ci-test && cargo ci-clippy"
            '';
          };
        });
    };
}
