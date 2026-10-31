# Fake XDG desktop portal network monitor for the test instance (SYNCATIVE_TEST_PORTAL=1 in scripts/test-instance.sh): always online, metered while the file given as the argument exists.
import os
import sys
import dbus, dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

IFACE = "org.freedesktop.portal.NetworkMonitor"
FLAG = sys.argv[1]
DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus)

class Monitor(dbus.service.Object):
    metered = os.path.exists(FLAG)
    @dbus.service.method(IFACE, out_signature="b")
    def GetAvailable(self): return True
    @dbus.service.method(IFACE, out_signature="b")
    def GetMetered(self): return Monitor.metered
    @dbus.service.signal(IFACE)
    def changed(self): pass

monitor = Monitor(bus, "/org/freedesktop/portal/desktop")
def poll():
    metered = os.path.exists(FLAG)
    if metered != Monitor.metered:
        Monitor.metered = metered
        monitor.changed()
    return True
GLib.timeout_add(300, poll)
GLib.MainLoop().run()
