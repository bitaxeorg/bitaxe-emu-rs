# rax at the revision the emulator builds with: only the files cargo reads.
{ fetchgit }:

fetchgit {
  url = "https://github.com/Georges760/rax.git";
  rev = "e395fe44df3795f29113914e4041cd294781fccf";
  hash = "sha256-9PGyDmGjxfQvquPtJLy1947u6Cuj8AJxkGLHmnvLszk=";
  sparseCheckout = [
    "/Cargo.toml"
    "/Cargo.lock"
    "/LICENSE"
    "/LICENSES"
    "/THIRD_PARTY_NOTICES.md"
    "/src"
    "/capi"
    "/vendor/vm-memory"
  ];
  nonConeMode = true;
}
