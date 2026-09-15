# ruff: noqa: I001, UP031
# pyright: reportAny=none, reportExplicitAny=none, reportUnusedCallResult=none, reportOptionalMemberAccess=none

import os, tomllib, re, shutil, subprocess, argparse, sys
from pathlib import Path

parser = argparse.ArgumentParser(
                    prog='create-deb-package.py',
                    description='Generate deb-file for AstraLite')
parser.add_argument('--arch')
parser.add_argument('--bin')
args = parser.parse_args()

# File names and directories

name = "AstraLite"
bin = "astra_lite"
icon = "astra_lite48x48.png"
this_path = os.path.dirname(os.path.realpath(__file__))
icon_file = os.path.join(this_path, "..", "src", "ui", "resources", icon)
if args.bin != None:
    bin_file = args.bin
else:
    bin_file = os.path.join(this_path, "..", "target", "release", bin)
if not os.path.isfile(bin_file):
    sys.exit("Binary not found: %s. Build it first with `cargo build --release` or pass --bin." % bin_file)
dist_dir = os.path.join(this_path, "..", "dist")
cargo_toml = os.path.join(this_path, "..", "Cargo.toml")
os.makedirs(dist_dir, exist_ok=True)
mapdata_in_dir = os.path.join(this_path, "..", "map_data")

# Package name and version from Cargo.toml

with open(cargo_toml, 'rb') as f:
    cargo = tomllib.load(f)
package_name = cargo['package']['name'].replace("_", "")
package_vers = cargo['package']['version']
description = cargo['package']['description']
vers_re = re.match(r'(\d+)\.(\d+)\.(\d+)', package_vers)
if vers_re is None:
    sys.exit("Invalid version in Cargo.toml: %s" % package_vers)
package_vers = vers_re.group(1) + '.' + vers_re.group(2) + '-' + vers_re.group(3)
bin_dir='opt/'+package_name

# Processor architecture

if args.arch != None:
    arch = args.arch
else:
    arch = subprocess.check_output([
        'dpkg',
        '--print-architecture'
    ]).decode("utf-8", 'ignore').strip()

# Full package file name and directory

package_file = '%s_%s_%s' % (package_name, package_vers, arch)
package_dir = os.path.join(dist_dir, package_file)
os.makedirs(package_dir, exist_ok=True)
debian_folder = os.path.join(package_dir, "DEBIAN")
os.makedirs(debian_folder, exist_ok=True)
full_bin_dir = os.path.join(package_dir, bin_dir)
os.makedirs(full_bin_dir, exist_ok=True)
shutil.copy(bin_file, full_bin_dir)
shutil.copy(icon_file, full_bin_dir)
shutil.copytree(mapdata_in_dir, os.path.join(full_bin_dir, "data"), dirs_exist_ok=True)

# Desktop entry

desktop_file_data = r'''[Desktop Entry]
Version=${vers}
Type=Application
Name=${name}
Comment=${descr}
Categories=Education;Science
TryExec=${bin}
Exec=${bin}
Icon=${icon}
'''
desktop_file_data = desktop_file_data.replace("${vers}", package_vers)
desktop_file_data = desktop_file_data.replace("${name}", name)
desktop_file_data = desktop_file_data.replace("${descr}", description)
desktop_file_data = desktop_file_data.replace("${bin}", '/'+os.path.join(bin_dir, bin))
desktop_file_data = desktop_file_data.replace("${icon}", '/'+os.path.join(bin_dir, icon))

desktop_dir = os.path.join(package_dir, "usr", "share", "applications");
os.makedirs(desktop_dir, exist_ok=True)
desktop_file = os.path.join(desktop_dir, "%s.desktop" % bin);
with open(desktop_file, "w") as text_file:
    text_file.write(desktop_file_data)

# Binaries size

files_size = sum(f.stat().st_size for f in Path(full_bin_dir).glob('**/*') if f.is_file())

# Dependicies

dep_debian_folder = os.path.join(package_dir, 'debian')
os.makedirs(dep_debian_folder, exist_ok=True)
dep_control_file = os.path.join(dep_debian_folder, "control")
with open(dep_control_file, 'w') as f:
    f.write('Source: %s\n' % package_name)
    f.write('Version: %s\n' % package_vers)
    f.write('Architecture: %s\n' % arch)
os.chdir(package_dir)
shlibdeps_res = subprocess.check_output([
    'dpkg-shlibdeps',
    '-O',
    os.path.join(bin_dir, bin)
])
dependices = shlibdeps_res.decode('utf-8', 'ignore').replace('shlibs:Depends=', '').strip()
shutil.rmtree(dep_debian_folder, ignore_errors=True)

# Control file

control_file = os.path.join(debian_folder, "control")
with open(control_file, 'w') as f:
    f.write('Package: %s\n' % package_name)
    f.write('Version: %s\n' % package_vers)
    f.write('Architecture: %s\n' % arch)
    f.write('Maintainer: Denis Artemov (denis.artyomov@gmail.com)\n')
    f.write('Depends: %s\n' % dependices)
    f.write('Installed-Size: %d\n' % int(files_size/1024))
    f.write('Description: %s\n' % description)

# Dirs file

dirs_file = os.path.join(debian_folder, "dirs")

with open(os.path.join(dirs_file), 'w') as f:
    f.write('/%s\n' % bin_dir)

# Generate package

subprocess.check_output([
    'dpkg-deb',
    '--root-owner-group',
    '--build',
    '-Zgzip',
    package_dir
])

shutil.rmtree(package_dir, ignore_errors=True)
