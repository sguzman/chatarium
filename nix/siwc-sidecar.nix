{ lib, stdenv, fetchurl, nodejs_22, autoPatchelfHook, devkit, bridgeSource }:

let
  # The pinned upstream lock is the authority for each tarball, including
  # transitive dependencies. No registry resolution or installer at runtime.
  lock = builtins.fromJSON (builtins.readFile (devkit + "/package-lock.json"));
  runtimeNames = [ "jose" "proper-lockfile" "graceful-fs" "retry" "signal-exit" ];
  buildNames = [
    "@types/proper-lockfile" "@types/retry" "@types/node" "undici-types"
    "typescript" "@typescript/typescript-linux-x64"
  ];
  unpack = name:
    let
      module = lock.packages."node_modules/${name}";
      archive = fetchurl { url = module.resolved; hash = module.integrity; };
    in ''
      mkdir -p node_modules/${lib.escapeShellArg name}
      tar --extract --gzip --file=${archive} --strip-components=1 \
        --directory=node_modules/${lib.escapeShellArg name}
    '';
in
stdenv.mkDerivation {
  pname = "chatarium-siwc-sidecar";
  version = "0.1.0-f723814";
  src = devkit;
  nativeBuildInputs = [ nodejs_22 autoPatchelfHook ];
  buildInputs = [ stdenv.cc.cc.lib ];
  strictDeps = true;

  buildPhase = ''
    runHook preBuild
    ${lib.concatMapStringsSep "\n" unpack (runtimeNames ++ buildNames)}
    # TypeScript 7 ships a native Linux compiler. Patch it before execution
    # (the installed sidecar does not include the compiler).
    autoPatchelf node_modules/@typescript/typescript-linux-x64
    cd packages/local
    node ../../node_modules/typescript/bin/tsc -p tsconfig.json
    node ../../scripts/copy-package-notices.mjs dist
    cd ../..
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    dest="$out/vendor/openai-sign-in-with-chatgpt-devkit"
    mkdir -p "$dest/packages/local" "$dest/node_modules" "$out/tools/siwc-bridge"
    cp package.json package-lock.json "$dest/"
    cp packages/local/package.json "$dest/packages/local/"
    cp -r packages/local/dist "$dest/packages/local/"
    ${lib.concatMapStringsSep "\n" (name: ''
      cp -r node_modules/${name} "$dest/node_modules/"
    '') runtimeNames}
    # Only the runtime bridge and its deterministic smoke harness.
    cp ${bridgeSource}/{bridge,response-lane,agent-activity,retry-policy,web-search-evidence,smoke}.mjs "$out/tools/siwc-bridge/"
    test -f "$dest/packages/local/dist/index.js"
    runHook postInstall
  '';
}
