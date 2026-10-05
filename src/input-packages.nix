# Best-effort provenance from explicitly exported input packages. Evaluates paths;
# it does not realise outputs or claim ownership of overlay-only dependencies.
{ flake, inputs, system }:
let
  root = builtins.getFlake flake;
  collect = input:
    let
      attempt = builtins.tryEval (root.inputs.${input}.packages.${system} or {});
      packages = if attempt.success then attempt.value else {};
      names = builtins.attrNames packages;
      bounded = builtins.genList (i: builtins.elemAt names i) (if builtins.length names > 512 then 512 else builtins.length names);
      package = attribute:
        let
          value = builtins.tryEval (let p = packages.${attribute}; in
            if (p.type or "") == "derivation" then let result = {
              inherit input attribute;
              name = (builtins.parseDrvName p.name).name;
              version = (builtins.parseDrvName p.name).version;
              path = p.outPath;
            }; in builtins.deepSeq result result else null);
        in if value.success then value.value else null;
    in builtins.filter (p: p != null) (map package bounded);
in builtins.concatLists (map collect inputs)
