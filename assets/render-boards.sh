#!/usr/bin/env bash
# Renders the front of each Bitaxe board (the side with the display, which is
# also the side of the BOOT/RESET buttons except on the GammaHex) from its KiCad
# project into assets/<name>-front.png.
# Each board revision is exported from its git repository with `git archive`
# into a temporary directory and rendered from there, so the checkouts are left
# untouched. The render is trimmed to the board and its shadow, then scaled.
# Usage: assets/render-boards.sh [hardware directory] [board name...]
#   hardware directory: holds the KiCad repositories (default ../../hardware
#   from this script; "" keeps the default); board names: render only these
#   (default: all).
set -euo pipefail

assets=$(cd "$(dirname "$0")" && pwd)
hardware=$(cd "${1:-$assets/../../hardware}" && pwd)
shift $(($# > 0 ? 1 : 0))

# name        repository     git revision     board file                side   width height scale [kicad-cli options]
boards=(
  "gamma       bitaxeGamma    origin/main      bitaxeGamma.kicad_pcb     bottom 1400  2200   50%"
  "max         bitaxeMax      origin/max-v2.2  bitaxeMax.kicad_pcb       bottom 1400  2200   50%"
  "ultra-205   bitaxeUltra    origin/ultra-205 bitaxeUltra.kicad_pcb     bottom 1400  2200   50%"
  "ultra-207   bitaxeUltra    origin/ultra-207 bitaxeUltra.kicad_pcb     bottom 1400  2200   50%"
  "hex         ultraHex       c0230f8          bitaxeHex.kicad_pcb       top    2000  2850   50%"
  "supra-401   bitaxeSupra    origin/supra-401 bitaxeSupra.kicad_pcb     bottom 1400  2200   50%"
  "supra-403   bitaxeSupra    origin/supra-403 bitaxeSupra.kicad_pcb     bottom 1400  2200   50%"
  "suprahex    supraHex       origin/main      bitaxeHex.kicad_pcb       top    2000  2850   50%"
  "gammaturbo  BitaxeGT       origin/main      BitaxeGT.kicad_pcb        top    1600  2460   50%"
  "najaduo     naja-duo       origin/master    BitaxeNajaDuo.kicad_pcb   top    2950  2680   50%"
  "gammahex    BitaxeGammaHex origin/main      BitaxeGammaHex.kicad_pcb  top    2100  2810   50% --use-board-stackup-colors"
)
# hex: c0230f8 is the last revision printed 303 (ultraHex master has moved on to 304).
# gammahex: bitaxeorg/BitaxeGammaHex; its LCD sits on the top side, its BOOT/RESET
# buttons on the bottom side at the top edge, labelled on the top side; its stackup
# sets the solder mask colour (black).

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# KiCad's STEP reader (OpenCASCADE 7.6) crashes while fixing the solids of the
# ESP32-S3-WROOM-1U model of the Hex boards: read STEP models with the default
# shape fixes except the solid one.
mkdir -p "$tmp/occt"
cat > "$tmp/occt/STEP" <<'EOF'
FromSTEP.exec.op : FixShape
FromSTEP.FixShape.Tolerance3d : &Runtime.Tolerance
FromSTEP.FixShape.MaxTolerance3d : &Runtime.MaxTolerance
FromSTEP.FixShape.MinTolerance3d : 1.e-7
FromSTEP.FixShape.FixFreeShellMode : -1
FromSTEP.FixShape.FixFreeFaceMode : -1
FromSTEP.FixShape.FixFreeWireMode : -1
FromSTEP.FixShape.FixSameParameterMode : -1
FromSTEP.FixShape.FixSolidMode : 0
FromSTEP.FixShape.FixShellOrientationMode : -1
FromSTEP.FixShape.CreateOpenSolidMode : 0
FromSTEP.FixShape.FixShellMode : -1
FromSTEP.FixShape.FixFaceOrientationMode : -1
FromSTEP.FixShape.FixFaceMode : -1
FromSTEP.FixShape.FixWireMode : -1
FromSTEP.FixShape.FixOrientationMode : -1
FromSTEP.FixShape.FixAddNaturalBoundMode : -1
FromSTEP.FixShape.FixMissingSeamMode : -1
FromSTEP.FixShape.FixSmallAreaWireMode : -1
FromSTEP.FixShape.RemoveSmallAreaFaceMode : -1
FromSTEP.FixShape.FixIntersectingWiresMode : -1
FromSTEP.FixShape.FixLoopWiresMode : -1
FromSTEP.FixShape.FixSplitFaceMode : -1
FromSTEP.FixShape.AutoCorrectPrecisionMode : 1
FromSTEP.FixShape.ModifyTopologyMode : 0
FromSTEP.FixShape.ModifyGeometryMode : 1
FromSTEP.FixShape.ClosedWireMode : 1
FromSTEP.FixShape.PreferencePCurveMode : 1
FromSTEP.FixShape.FixReorderMode : -1
FromSTEP.FixShape.FixSmallMode : -1
FromSTEP.FixShape.FixConnectedMode : -1
FromSTEP.FixShape.FixEdgeCurvesMode : -1
FromSTEP.FixShape.FixDegeneratedMode : -1
FromSTEP.FixShape.FixLackingMode : -1
FromSTEP.FixShape.FixSelfIntersectionMode : -1
FromSTEP.FixShape.RemoveLoopMode : -1
FromSTEP.FixShape.FixReversed2dMode : -1
FromSTEP.FixShape.FixRemovePCurveMode : -1
FromSTEP.FixShape.FixRemoveCurve3dMode : -1
FromSTEP.FixShape.FixAddPCurveMode : -1
FromSTEP.FixShape.FixAddCurve3dMode : -1
FromSTEP.FixShape.FixSeamMode : -1
FromSTEP.FixShape.FixShiftedMode : -1
FromSTEP.FixShape.FixEdgeSameParameterMode : 0
FromSTEP.FixShape.FixNotchedEdgesMode : -1
FromSTEP.FixShape.FixTailMode : 0
FromSTEP.FixShape.MaxTailAngle : 0
FromSTEP.FixShape.MaxTailWidth : -1
FromSTEP.FixShape.FixSelfIntersectingEdgeMode : -1
FromSTEP.FixShape.FixIntersectingEdgesMode : -1
FromSTEP.FixShape.FixNonAdjacentIntersectingEdgesMode : -1
FromSTEP.FixShape.FixVertexPositionMode : 0
FromSTEP.FixShape.FixVertexToleranceMode : -1
EOF
export CSF_STEPDefaults=$tmp/occt

for row in "${boards[@]}"; do
  read -r name repo rev pcb side width height scale options <<<"$row"
  if (($# > 0)) && [[ " $* " != *" $name "* ]]; then
    continue
  fi
  if [[ ! -d $hardware/$repo ]]; then
    echo "skipped $name: no $repo in $hardware" >&2
    continue
  fi
  src=$tmp/$name
  mkdir -p "$src"
  git -C "$hardware/$repo" archive "$rev" | tar -x -C "$src"
  # shellcheck disable=SC2086 # options holds zero or more words
  kicad-cli pcb render --side "$side" --width "$width" --height "$height" \
    --background transparent --quality high $options -o "$tmp/$name.png" "$src/$pcb" >/dev/null
  out=$assets/$name-front.png
  nix-shell -p imagemagick --run "magick '$tmp/$name.png' -trim +repage -resize $scale '$out'"
  echo "wrote $out ($repo $rev, $side)"
done
