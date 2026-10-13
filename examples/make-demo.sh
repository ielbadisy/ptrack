#!/usr/bin/env bash
# Builds a fictional demo registry in examples/demo/.registry.
# Every project, venue, person and code here is invented.
#   ./examples/make-demo.sh && PTRACK_REGISTRY=examples/demo/.registry ptrack
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
demo="$here/demo"
rm -rf "$demo"
mkdir -p "$demo"
export PTRACK_REGISTRY="$demo/.registry"
pt() { ptrack "$@" >/dev/null; }
day() { date -d "$1 days" +%F; }

pt init "$demo"

pt new paper "Aurora: tidal forecasting with sparse buoys" --slug aurora --status active --priority high \
  --code ORCA --venue "Journal of Imaginary Oceans" --next "Answer reviewer 2 on the boundary model" \
  --collab "R. Moreau" --collab "L. Chen" --tag lab
pt submit aurora "Annals of Fiction" --date "$(day -120)"
pt outcome aurora rejected --date "$(day -60)" --note "scope mismatch"
pt submit aurora "Journal of Imaginary Oceans" --date "$(day -30)"
pt outcome aurora revision --date "$(day -5)"
pt deadline aurora "$(day 9)" revision due

pt new paper "Kelp canopy mapping from drone imagery" --slug kelp-maps --status active --code ORCA \
  --next "Finish the validation section" --tag lab
pt deadline kelp-maps "$(day -2)" internal draft to coauthors

pt new paper "Bayesian lighthouse placement" --slug lighthouse --status submitted --priority normal \
  --venue "Stochastic Harbours" --next "Wait for reviews" --tag lab
pt submit lighthouse "Stochastic Harbours" --date "$(day -21)"

pt new paper "A survey of synthetic tide tables" --slug tide-survey --status idea --priority low \
  --next "Collect candidate references" --tag reading-group

pt new software "quickknn: fast nearest neighbours for gappy data" --slug quickknn --status active \
  --priority high --code HELIX --venue CRAN --next "Fix the Windows build note" --tag cran --tag lab
pt deadline quickknn "$(day 4)" CRAN resubmission

pt new software "tidalr: tidal harmonics in R" --slug tidalr --status published --venue CRAN --tag cran

pt new software "seaglass: plotting themes for ocean data" --slug seaglass --status paused \
  --next "Decide whether to merge into tidalr"

pt new grant "Coastal sensing consortium" --slug coastal-grant --status active --priority high \
  --code ORCA --venue "Blue Science Fund" --next "Budget table and letters of support" \
  --collab "Harbour Institute" --tag lab
pt deadline coastal-grant "$(day 18)" full proposal

pt new course "Data analysis for marine scientists" --slug marine-course --status active --code TEACH \
  --next "Record lecture 5" --tag teaching
pt deadline marine-course "$(day 26)" final exam

pt new talk "Sparse sensors, dense forecasts" --slug sensors-talk --status planned \
  --venue "Imaginary Ocean Days" --next "Draft slides" --tag lab
pt deadline sensors-talk "$(day 40)" talk

pt new book "Ocean statistics with R" --slug ocean-book --status active --priority low \
  --code TEACH --next "Chapter 4: mixed models" --tag teaching

pt new dataset "Harbour buoy archive 2010-2025" --slug buoy-archive --status accepted \
  --venue "Data in Fiction" --code HELIX --next "Upload final files" --tag lab

pt new consulting "Fish-stock sample size review" --slug fishstock-review --status active \
  --collab "North Bay Fisheries" --next "Send power calculation"

pt new collab "Reef acoustics pilot" --slug reef-acoustics --status planned --code ORCA \
  --collab "J. Okafor" --tag lab

pt new other "Supervision: summer intern" --slug intern-2026 --status active --tag teaching \
  --next "Weekly check-in"

pt new website "Lab website" --slug lab-site --status archived

pt link kelp-maps uses quickknn
pt link aurora uses quickknn
pt link sensors-talk presents aurora

echo "demo registry: $PTRACK_REGISTRY ($(ls "$PTRACK_REGISTRY" | grep -c '^p') projects)"
