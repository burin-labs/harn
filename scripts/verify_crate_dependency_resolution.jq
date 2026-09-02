def exactly_one($label):
  if type != "array" or length != 1 then
    error("expected exactly one " + $label)
  else
    .[0]
  end;

. as $metadata
| ([$metadata.packages[] | select(.name == $package and .version == $package_version) | .id]
    | exactly_one("packaged source node")) as $source_id
| ([$metadata.resolve.nodes[]
    | select(.id == $source_id)
    | .deps[]
    | select(.name == $resolution_name)
    | .pkg]
    | exactly_one("dependency resolution edge")) as $dependency_id
| ([$metadata.packages[] | select(.id == $dependency_id) | .version]
    | exactly_one("resolved dependency package"))
