# APK Releases

Input Dynamics Keyboard publishes debug APKs as GitHub Release assets. Do not
commit APK, AAB, APKS, keystore, signing output, or checksum files to git.

## Release Assets

Each release should include:

- signed debug APK for `org.inputdynamics.ime.debug`
- `SHA256SUMS.txt`
- `APK_PERMISSIONS.txt`
- `APK_SIGNING_CERT_SHA256.txt`

The debug APK is the supported install artifact for local research and agent
automation workflows. A signed release APK for `org.inputdynamics.ime` can be
added later if the project needs a stable non-debug package identity.

## Signing

Installable APKs must be signed with the project APK signing key. The keystore
is private material and must not be committed. Local trusted builds load signing
configuration from `.git/signing/input-dynamics.env`; GitHub Actions reconstructs
the same keystore from repository secrets.

Required local environment variables:

```text
INPUT_DYNAMICS_KEYSTORE_FILE
INPUT_DYNAMICS_KEYSTORE_TYPE
INPUT_DYNAMICS_KEYSTORE_PASSWORD
INPUT_DYNAMICS_KEY_ALIAS
INPUT_DYNAMICS_KEY_PASSWORD
```

The Nix dev shell loads `.git/signing/input-dynamics.env` automatically when it
exists. Gradle refuses to run installable artifact tasks such as `assembleDebug`
unless signing is configured.

## Publishing

Use the release helper:

```bash
tools/release.py 0.1.0 --push
```

The helper updates Android version metadata, runs release verification, commits
the version bump, creates an annotated tag, and can push `main` plus the tag.
The workflow can also be run manually from GitHub Actions with an existing tag.
Initial public releases should stay marked as prereleases until a clean-device
install, session start, keypress, stop, pull, and JSONL validation pass has been
completed.

## Versioning

Use SemVer tags for the fork:

```text
v0.1.0
v0.1.1
v0.2.0
```

Android `versionName` includes upstream provenance as build metadata:

```text
0.1.0+heli3.9
```

GitHub Release APK assets include the same provenance:

```text
InputDynamicsKeyboard-v0.1.0-heli3.9-debug.apk
```

`versionCode` is derived from SemVer as `100000 + major * 1000000 + minor *
1000 + patch`, so `0.1.0` becomes `101000`. The offset keeps public fork APKs
above earlier 3.9-based local debug builds.

## Release Verification

The workflow runs:

```bash
./gradlew :app:testRunTestsUnitTest :app:assembleDebug
```

It then checks the debug APK for absence of `android.permission.INTERNET` and
writes checksums plus the APK signing certificate fingerprint.

Local release builds use the same project signing key when signing is configured:

```bash
./gradlew :app:assembleRelease
```

```text
app/build/outputs/apk/release/*-release.apk
```
