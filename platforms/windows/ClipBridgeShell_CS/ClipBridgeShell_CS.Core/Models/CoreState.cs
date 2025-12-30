using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace ClipBridgeShell_CS.Core.Models;

public enum CoreState
{
    NotLoaded,
    Loading,
    Ready,
    Degraded,
    ShuttingDown,
}
