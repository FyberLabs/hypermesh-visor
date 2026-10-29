"""Small real Tk editor used by the X11 scroll acceptance scenario."""
import tkinter as tk

root = tk.Tk()
root.title('Visor scroll editor')
root.geometry('500x400+600+40')
editor = tk.Text(root, wrap='none')
editor.pack(fill='both', expand=True)
editor.insert('1.0', '\n'.join(f'Line {i}: scroll acceptance text' for i in range(200)))
editor.configure(yscrollcommand=lambda first, last: print(first, flush=True))
root.mainloop()
