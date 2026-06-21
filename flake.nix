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
              pkgs.git
              pkgs.jdk17
              pkgs.unzip
              pkgs.zip
            ];

            ANDROID_HOME = androidHome;
            ANDROID_SDK_ROOT = androidHome;
            ANDROID_NDK_HOME = ndkHome;
            ANDROID_NDK_ROOT = ndkHome;
            JAVA_HOME = pkgs.jdk17.home;

            shellHook = ''
              export ANDROID_USER_HOME="$PWD/.android"
              export PATH="${androidHome}/platform-tools:${buildTools}:${ndkHome}:$PATH"

              echo "Android SDK: $ANDROID_HOME"
              echo "Android NDK: $ANDROID_NDK_HOME"
              echo "Java: $JAVA_HOME"
              echo "Build with: ./gradlew :app:assembleDebugNoMinify"
            '';
          };
        });
    };
}
