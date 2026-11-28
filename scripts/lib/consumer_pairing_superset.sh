#!/usr/bin/env bash

# The reader fills a named variable from one authenticated repository API read.
# This owner checks containment; callers retain their credential and retry policy.
consumer_pairing_superset_check() {
  local repository=$1 paired=$2 rehearsed=$3 superseded=$4 reader=$5
  local pull seen='' head comparison status base
  CONSUMER_SUPERSET_REASON=pairing_superset_unmeasured
  CONSUMER_SUPERSET_DETAIL=''
  [[ "$paired" =~ ^[1-9][0-9]*$ ]] || return 1
  CONSUMER_SUPERSET_DETAIL="pull=$paired"
  [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ &&
     "$rehearsed" =~ ^[0-9a-f]{40}$ &&
     "$superseded" =~ ^[1-9][0-9]*(,[1-9][0-9]*)*$ ]] || return 1
  for pull in ${superseded//,/ }; do
    [[ "$pull" != "$paired" && ",$seen," != *",$pull,"* ]] || return 1
    seen="${seen:+$seen,}$pull"
  done
  for pull in ${superseded//,/ }; do
    CONSUMER_SUPERSET_DETAIL="pull=$paired superseded=$pull"
    head=''
    "$reader" head "$repository" "/pulls/$pull" .head.sha || return 1
    [[ "$head" =~ ^[0-9a-f]{40}$ ]] || return 1
    comparison=''
    "$reader" comparison "$repository" "/compare/$head...$rehearsed" \
      '"\(.status) \(.merge_base_commit.sha)"' || return 1
    [[ "$comparison" =~ ^(identical|ahead|behind|diverged)\ ([0-9a-f]{40})$ ]] || return 1
    read -r status base <<< "$comparison"
    if [[ ! ( "$status" == identical || "$status" == ahead ) || "$base" != "$head" ]]; then
      CONSUMER_SUPERSET_REASON=pairing_superset_missing
      CONSUMER_SUPERSET_DETAIL="pull=$paired missing=$pull"
      return 1
    fi
  done
}
