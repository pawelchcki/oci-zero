#!/usr/bin/env bash
# Turn an OCI image layout into the tree a registry's *read* path is made of, so object
# storage can serve `crane pull` with no server process.
#
# The OCI distribution spec's read path is three GET shapes and nothing else:
#
#   GET /v2/                                 -> 200, any JSON body
#   GET /v2/<name>/manifests/<tag|digest>    -> the manifest bytes, Content-Type = its mediaType
#   GET /v2/<name>/blobs/<digest>            -> the blob bytes
#
# Lay those out as keys and a bucket is a registry. The write path — /v2/.../blobs/uploads/,
# with its POST/PATCH/PUT session dance — is not implemented and is not meant to be:
# publishing is `aws s3 sync` of the directory this script produces, which is why CI needs
# `s3:PutObject` rather than anything resembling registry credentials.
#
#   tools/publish-static-registry.sh --layout layout --name oci-zero/firmware \
#       --tag latest --output publish
#   aws s3 sync publish/v2/oci-zero/firmware/ s3://$BUCKET/v2/oci-zero/firmware/ --delete
#
# Two outputs, because content type is not decoration here. A manifest served as
# application/octet-stream is not a manifest as far as a client is concerned, and `aws s3
# sync` can only set one content type for a whole run. So the blobs — many, uniformly
# opaque — go up in one sync, and the manifests — few, each with its own media type — are
# listed in `uploads.tsv` for the caller to `aws s3 cp` individually.
#
# Bash rather than the POSIX sh of build-firmware-artifact.sh: an index can nest, so the
# walk is recursive, and recursion with local state is what a function-local `local` buys.
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
Usage: publish-static-registry.sh --layout DIR --name NAME [options]

Required:
  --layout DIR       OCI image layout directory, as built by build-firmware-artifact.sh
  --name NAME        repository name, the <name> in /v2/<name>/... . May contain slashes.

Options:
  --tag TAG          publish the layout's index under this tag. Repeatable.
                     Default: latest
  --output DIR       tree to create (default: ./publish)

Writes:
  DIR/v2/<name>/manifests/<digest|tag>
  DIR/v2/<name>/blobs/<digest>
  DIR/v2/<name>/tags/list
  DIR/uploads.tsv    relative-path <TAB> content-type <TAB> cache-control
                     for every object that must NOT be uploaded as octet-stream
EOF
    exit 2
}

layout=
name=
output=publish
tags=()

while [ $# -gt 0 ]; do
    case "$1" in
        --layout) layout=${2?missing value for --layout}; shift 2 ;;
        --name)   name=${2?missing value for --name}; shift 2 ;;
        --tag)    tags+=("${2?missing value for --tag}"); shift 2 ;;
        --output) output=${2?missing value for --output}; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done

[ -n "$layout" ] || usage
[ -n "$name" ] || usage
[ "${#tags[@]}" -gt 0 ] || tags=(latest)
[ -f "$layout/index.json" ] || { echo "not an OCI layout, no index.json: $layout" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

# A name with a leading or trailing slash produces `//` in a key, which S3 stores happily
# and no client can ever ask for again.
case "$name" in
    /*|*/) echo "--name must not begin or end with a slash: $name" >&2; exit 1 ;;
esac

sha256_hex() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

root="$output/v2/$name"
uploads="$output/uploads.tsv"
rm -rf "$output"
mkdir -p "$root/manifests" "$root/blobs" "$root/tags"
: > "$uploads"

# Content-addressed, therefore immutable, therefore cacheable forever. This is what keeps
# the CloudFront bill at zero on the repeat pulls that dominate: an edge that already holds
# sha256:abc never revalidates it, because there is no version of sha256:abc but one.
immutable='public, max-age=31536000, immutable'
# A tag is the one mutable name in the system, so it gets a short TTL. The publisher also
# invalidates it, which makes a republish visible immediately rather than within the minute;
# the TTL is the backstop for when an invalidation is skipped or fails.
mutable='public, max-age=60'

# Records an object that needs a content type other than application/octet-stream.
record() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$uploads"
}

blob_path() {
    printf '%s/blobs/sha256/%s\n' "$layout" "${1#sha256:}"
}

# Blobs keep their digest as the filename, colon and all. A colon is a legal S3 key
# character and a legal URL path character (RFC 3986 pchar), so `/v2/x/blobs/sha256:abc`
# needs no escaping anywhere along the path. They are deliberately NOT recorded in
# uploads.tsv: every blob is opaque bytes to the transport, and one bulk sync at
# application/octet-stream is both correct and the only way this stays one API call per file.
copy_blob() {
    local digest=$1 source
    source=$(blob_path "$digest")
    [ -f "$source" ] || { echo "layout is missing blob $digest" >&2; exit 1; }
    cp -f "$source" "$root/blobs/$digest"
}

# Manifests are recorded, because a client dispatches on Content-Type. Serving an index as
# application/vnd.oci.image.manifest.v1+json makes it undecodable, and serving either as
# octet-stream makes it invisible.
copy_manifest() {
    local digest=$1 media_type=$2 source
    source=$(blob_path "$digest")
    [ -f "$source" ] || { echo "layout is missing manifest $digest" >&2; exit 1; }
    cp -f "$source" "$root/manifests/$digest"
    record "v2/$name/manifests/$digest" "$media_type" "$immutable"
}

# Walks one manifest and everything it references. Indexes nest, so this recurses; the
# layout is a finite DAG built by the same tool that wrote index.json, so there is no cycle
# to guard against — a cycle would require a blob containing its own digest.
walk_manifest() {
    local digest=$1 media_type=$2 source
    copy_manifest "$digest" "$media_type"
    source=$(blob_path "$digest")

    case "$media_type" in
        *image.index.v1+json|*manifest.list.v2+json)
            while IFS=$'\t' read -r child_digest child_type; do
                [ -n "$child_digest" ] || continue
                walk_manifest "$child_digest" "$child_type"
            done < <(jq -r '.manifests[]? | [.digest, .mediaType] | @tsv' "$source")
            ;;
        *)
            # config and layers. `.config` is absent on some artifact manifests, and jq's
            # `?` keeps that from being an error rather than an empty result.
            while IFS= read -r child_digest; do
                [ -n "$child_digest" ] || continue
                copy_blob "$child_digest"
            done < <(jq -r '[.config?.digest // empty] + [.layers[]?.digest] | .[]' "$source")

            # A referrer's subject is a manifest that must already exist in the registry.
            # It is not part of this layout's closure, so it is checked rather than copied:
            # publishing a dangling reference is the failure that shows up much later, as a
            # referrers query returning something unresolvable.
            local subject
            subject=$(jq -r '.subject?.digest // empty' "$source")
            if [ -n "$subject" ] && [ ! -f "$(blob_path "$subject")" ]; then
                echo "manifest $digest names subject $subject, which is not in the layout" >&2
                exit 1
            fi
            ;;
    esac
}

index_type=$(jq -r '.mediaType // "application/vnd.oci.image.index.v1+json"' "$layout/index.json")
index_hex=$(sha256_hex "$layout/index.json")
index_digest="sha256:$index_hex"

# The index is the layout's root but is not itself in blobs/, so it is placed by hand and
# then walked as if it had been.
cp -f "$layout/index.json" "$root/manifests/$index_digest"
record "v2/$name/manifests/$index_digest" "$index_type" "$immutable"

while IFS=$'\t' read -r child_digest child_type; do
    [ -n "$child_digest" ] || continue
    walk_manifest "$child_digest" "$child_type"
done < <(jq -r '.manifests[]? | [.digest, .mediaType] | @tsv' "$layout/index.json")

# The tags. Each is a copy of the index bytes, not a redirect: the read path has no
# indirection to offer, and a 30-line JSON document duplicated per tag is cheaper than any
# mechanism that would avoid it.
for tag in "${tags[@]}"; do
    case "$tag" in
        *[!A-Za-z0-9._-]*|[!A-Za-z0-9_]*)
            echo "invalid tag (must match [A-Za-z0-9_][A-Za-z0-9._-]*): $tag" >&2
            exit 1
            ;;
    esac
    cp -f "$layout/index.json" "$root/manifests/$tag"
    record "v2/$name/manifests/$tag" "$index_type" "$mutable"
done

# GET /v2/<name>/tags/list. Only the tags this run published: the tree is uploaded with
# `aws s3 sync --delete` scoped to this repository's prefix, so a publish replaces the
# repository wholesale. That is the garbage collection — there is no lifecycle rule that
# could safely expire a blob, because a blob is shared by every manifest that names it.
# The consequence to know about: publishing one tag drops every tag not passed to this run.
printf '%s' "$(jq -nc --arg name "$name" --args '{name: $name, tags: $ARGS.positional}' -- "${tags[@]}")" \
    > "$root/tags/list"
record "v2/$name/tags/list" "application/json" "$mutable"

blob_count=$(find "$root/blobs" -type f | wc -l | tr -d ' ')
manifest_count=$(find "$root/manifests" -type f | wc -l | tr -d ' ')

{
    echo "Published tree $root"
    echo "  repository   $name"
    echo "  index        $index_digest ($index_type)"
    echo "  tags         ${tags[*]}"
    echo "  manifests    $manifest_count"
    echo "  blobs        $blob_count"
} >&2

# Emitted last and on stdout so a caller can capture it without the summary above.
echo "index_digest=$index_digest"
