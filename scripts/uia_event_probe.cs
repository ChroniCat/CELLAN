using System;
using System.Diagnostics;
using System.Threading;
using System.Windows.Forms;
using System.Windows.Automation;

class Probe {
    [STAThread]
    static void Main(string[] args) {
        int pid = int.Parse(args[0]);
        AutomationElement win = null;
        for (int i = 0; i < 100 && win == null; i++) {
            win = AutomationElement.RootElement.FindFirst(TreeScope.Children,
                new PropertyCondition(AutomationElement.ProcessIdProperty, pid));
            if (win == null) Thread.Sleep(100);
        }
        if (win == null) { Console.WriteLine("NO-WINDOW"); return; }
        Console.WriteLine("WINDOW " + win.Current.Name);
        Automation.AddAutomationEventHandler(
            SelectionItemPattern.ElementSelectedEvent,
            win, TreeScope.Subtree,
            (s, e) => {
                var el = s as AutomationElement;
                string name = ""; string ct = "";
                try { name = el.Current.Name; ct = el.Current.ControlType.ProgrammaticName; } catch {}
                Console.WriteLine("EVENT " + ct + " | " + name);
            });
        Automation.AddAutomationFocusChangedEventHandler((s, e) => {
            var el = s as AutomationElement;
            string name = ""; try { name = el.Current.Name; } catch {}
            Console.WriteLine("FOCUS " + name);
        });
        var t = new System.Windows.Forms.Timer(); t.Interval = 20000;
        t.Tick += (s, e) => { Application.Exit(); };
        t.Start();
        Application.Run();
        Console.WriteLine("DONE");
    }
}
