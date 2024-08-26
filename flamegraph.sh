#!/bin/bash

# Check if input file name is provided
if [ -z "$1" ]; then
    echo "Usage: $0 <input>"
    exit 1
fi

# Assign the input file name to a variable
postfix="$1"

# Ensure the input file is readable
sudo chmod +r "read.data"
sudo chmod +r "write.data"

# Run the flamegraph command
flamegraph --perfdata "./read.data" -o "read_${postfix}.svg"
flamegraph --perfdata "./write.data" -o "write_${postfix}.svg"

