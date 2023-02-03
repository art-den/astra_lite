import os, configparser, re, shutil, subprocess
from pathlib import Path

# File names and directories

name = "AstraLite"
bin = "astra_lite"
icon = "astra_lite48x48.png"
this_path = os.path.dirname(os.path.realpath(__file__))
icon_file = os.path.join(this_path, "..", "ui", icon)
bin_file = os.path.join(this_path, "..", "target", "release", bin)
dist_dir = os.path.join(this_path, "..", "dist")
cargo_toml = os.path.join(this_path, "..", "Cargo.toml")
os.makedirs(dist_dir, exist_ok=True)

# Package name and version from Cargo.toml

config = configparser.ConfigParser()
config.read(cargo_toml)
package_name = config['package']['name'].replace('"', '').replace("_", "")
package_vers = config['package']['version'].replace('"', '')
description = config['package']['description'].replace('"', '')
vers_re = re.match('(\d+)\.(\d+)\.(\d+)', package_vers)
package_vers = vers_re.group(1) + '.' + vers_re.group(2) + '-' + vers_re.group(3)
bin_dir='opt/'+package_name

# Processor architecture

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

# Desktop entry

desktop_file_data = r'''[Desktop Entry]
Version=${vers}
Type=Application
Name=${name}
Comment=${descr}
Categories=Graphics;Astronomy
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
    package_dir
])

shutil.rmtree(package_dir, ignore_errors=True)