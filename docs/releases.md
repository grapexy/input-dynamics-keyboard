# APK Releases

Input Dynamics Keyboard publishes debug APKs as GitHub Release assets. Do not
commit APK, AAB, APKS, keystore, signing output, or checksum files to git.

## Release Assets

Each release should include:

- debug APK for `org.inputdynamics.ime.debug`
- `SHA256SUMS.txt`
- `APK_PERMISSIONS.txt`

The debug APK is the supported install artifact for local research and agent
automation workflows. A signed release APK for `org.inputdynamics.ime` can be
added later if the project needs a stable non-debug package identity.

## Publishing

Create and push a tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The workflow can also be run manually from GitHub Actions with an existing tag.
Initial public releases should stay marked as prereleases until a clean-device
install, session start, keypress, stop, pull, and JSONL validation pass has been
completed.

## Release Verification

The workflow runs:

```bash
./gradlew :app:testDebugUnitTest :app:assembleDebug
```

It then checks the debug APK for absence of `android.permission.INTERNET` and
writes checksums.

Local unsigned release builds are still useful for smoke testing the release
build type, but they are not published by the current GitHub Release workflow:

```bash
./gradlew :app:assembleRelease
```

```text
app/build/outputs/apk/release/InputDynamicsKeyboard_3.9-release-unsigned.apk
```
