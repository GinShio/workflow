#!/usr/bin/env bash

filename=$1
echo ${filename:?Missing filename} >/dev/null
sed -i -E '/^[[:space:]]*$/d; s/[[:space:]]+$//g' $filename
contents=$(<$filename)
truncate -s 0 $filename
max_length=$(awk '{ if (length($0) > max) max = length($0) } END { print max }' <<<"$contents")
while IFS= read -r line; do
    printf '%s%*s\n' "$line" "$(($max_length-${#line}))" "" >>$filename
done <<<"$contents"
