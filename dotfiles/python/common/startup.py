#!/usr/bin/env python3

import atexit
import os
import readline

xdg_cache_home = os.getenv('XDG_CACHE_HOME')
histfile = os.path.join(xdg_cache_home, "history_python")

try:
    readline.read_history_file(histfile)
    readline.set_history_length(10000)
except FileNotFoundError:
    pass

atexit.register(readline.write_history_file, histfile)
