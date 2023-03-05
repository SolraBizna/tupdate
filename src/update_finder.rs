use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry as HashMapEntry},
    env,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use mlua::{
    Lua,
    FromLua,
    Function,
    MultiValue,
    Table,
    ThreadStatus,
    Value::Nil,
};
use url::Url;
use wax::Glob;

use super::*;

fn sense(anchor: &Path, srcglob: &str) -> mlua::Result<bool> {
    let (glob, wants_dir) = if srcglob.ends_with("/") {
        (&srcglob[..srcglob.len()-1], true)
    } else { (&srcglob[..], false) };
    let glob = match Glob::new(&glob) {
        Ok(glob) => glob,
        Err(_) => {
            return Err(mlua::Error::RuntimeError(format!("Syntactically invalid glob among dir sense globs")));
        },
    };
    if glob.has_root() || glob.has_semantic_literals() {
        return Err(mlua::Error::RuntimeError(format!("Forbidden glob among dir sense globs. Rooted globs, and semantic components (such as \"..\"), are not allowed"))); 
    }
    if let Some(Ok(q)) = glob.walk(anchor).next() {
        if q.file_type().is_dir() != wants_dir {
            return Ok(false);
        }
    }
    else {
        return Ok(false);
    }
    Ok(true)
}

struct UpdateFinder {
    gui: Rc<RefCell<dyn Gui>>,
    verbose: bool,
    dirs: HashMap<String, PathBuf>,
    basedir: Option<PathBuf>,
    url: Url,
    installs: Vec<(PathBuf, Url)>,
    deletes: HashMap<PathBuf, Vec<String>>,
}

impl UpdateFinder {
    fn new(gui: Rc<RefCell<dyn Gui>>, verbose: bool, url: Url) -> UpdateFinder {
        UpdateFinder {
            gui,
            verbose,
            dirs: HashMap::new(),
            basedir: None,
            url,
            installs: vec![],
            deletes: HashMap::new(),
        }
    }
}

trait UpdateFinderRef {
    fn refmut(&self) -> mlua::Result<std::cell::RefMut<UpdateFinder>>;
    fn refconst(&self) -> mlua::Result<std::cell::Ref<UpdateFinder>>;
    fn check_detected_dir(&self, var: &str, candidate: &Path, silhouette: &Table) -> mlua::Result<bool>;
    fn detect_dir(&self, lua: &Lua, id: String, name: String, candidate_iter: Function, silhouette: Table) -> mlua::Result<()>;
    fn basedir(&self, lua: &Lua, target: String) -> mlua::Result<()>;
    fn cd(&self, lua: &Lua, target: String) -> mlua::Result<()>;
    fn sense(&self, _lua: &Lua, target: String) -> mlua::Result<bool>;
    fn install(&self, _lua: &Lua, target: String) -> mlua::Result<()>;
    fn delete_unmatched(&self, _lua: &Lua, target: String) -> mlua::Result<()>;
}

impl UpdateFinderRef for Rc<RefCell<UpdateFinder>> {
    fn refmut(&self) -> mlua::Result<std::cell::RefMut<UpdateFinder>> {
        Ok(self.try_borrow_mut().expect("Attempt to perform unsafe borrow on UpdateFinder"))
    }
    fn refconst(&self) -> mlua::Result<std::cell::Ref<UpdateFinder>> {
        Ok(self.try_borrow().expect("Attempt to perform unsafe borrow on UpdateFinder"))
    }
    fn check_detected_dir(&self, var: &str, candidate: &Path, silhouette: &Table) -> mlua::Result<bool> {
        let verbose = self.refconst()?.verbose;
        if !candidate.is_absolute() {
            return Err(mlua::Error::RuntimeError(format!("Path is invalid (must be absolute)"))); 
        }
        let mut ok = true;
        if let Ok(globs) = silhouette.get::<_, Vec<String>>("sense") {
            for srcglob in globs.iter() {
                if !sense(candidate, srcglob)? {
                    if verbose {
                        self.refconst()?.gui.borrow_mut().verbose(&format!("    Rejected: doesn't match glob {:?}", srcglob));
                    }            
                    ok = false;
                }
            }
        }
        if ok {
            if verbose {
                self.refconst()?.gui.borrow_mut().verbose(&format!("    Accepted!"));
            }
            self.refmut()?.dirs.insert(var.to_string(), candidate.to_path_buf());
        }
        Ok(ok)
    }
    fn detect_dir(&self, lua: &Lua, id: String, name: String, candidate_iter: Function, silhouette: Table) -> mlua::Result<()> {
        let verbose = self.refconst()?.verbose;
        if self.refconst()?.dirs.contains_key(&id) {
            return Ok(())
        }
        if verbose {
            self.refconst()?.gui.borrow_mut().verbose(&format!("Detecting {:?} ({}):", id, name));
        }
        if let Some(wo) = env::var_os(&id) {
            if verbose {
                self.refconst()?.gui.borrow_mut().verbose(&format!("  Environment variable: {:?}", wo));
            }
            if self.check_detected_dir(&id, &Path::new(&wo), &silhouette)? { return Ok(()) }
        }
        let cor = lua.create_thread(candidate_iter)?;
        while cor.status() == ThreadStatus::Resumable {
            let candidate: Option<String> = cor.resume(())?;
            match candidate {
                Some(wo) => {
                    if verbose {
                        self.refconst()?.gui.borrow_mut().verbose(&format!("  Index suggests: {:?}", wo));
                    }
                    if self.check_detected_dir(&id, &Path::new(&wo), &silhouette)? { return Ok(()) }
                },
                None => break,
            }
        }
        Ok(())
    }
    fn basedir(&self, _lua: &Lua, target: String) -> mlua::Result<()> {
        let dir = match self.refconst()?.dirs.get(&target) {
            None => {
                return Err(mlua::Error::RuntimeError(format!("No detected base directory identified as {:?} found. Use `detect_dir` before calling basedir.", target)));
            },
            Some(x) => x.clone(),
        };
        if self.refconst()?.verbose {
            self.refconst()?.gui.borrow_mut().verbose(&format!("Entering {:?} ({})", dir, target));
        }
        self.refmut()?.basedir = Some(dir);
        Ok(())
    }
    fn cd(&self, _lua: &Lua, target: String) -> mlua::Result<()> {
        if is_fishy_path(&target) {
            return Err(mlua::Error::RuntimeError(format!("You cannot cd to an absolute path, or use any path component that starts with a .")));
        }
        let mut me = self.refmut()?;
        if let Some(basedir) = me.basedir.as_mut() {
            basedir.push(&target);
        }
        else {
            return Err(mlua::Error::RuntimeError(format!("You must use basedir before you can cd")));
        }
        if me.verbose {
            me.gui.borrow_mut().verbose(&format!("Entering {:?}", me.basedir.as_ref().unwrap()));
        }
        Ok(())
    }
    fn sense(&self, _lua: &Lua, target: String) -> mlua::Result<bool> {
        let mut me = self.refmut()?;
        if let Some(basedir) = me.basedir.as_mut() {
            sense(basedir, &target)
        }
        else {
            Err(mlua::Error::RuntimeError(format!("You must use basedir before you can cd")))
        }
    }
    fn install(&self, _lua: &Lua, target: String) -> mlua::Result<()> {
        let mut me = self.refmut()?;
        let url = me.url.join(&target).map_err(|_| {
            mlua::Error::RuntimeError(format!("Install parameter must be a valid URL"))
        })?;
        let basedir = if let Some(basedir) = me.basedir.as_ref() { basedir.clone() }
        else {
            return Err(mlua::Error::RuntimeError(format!("You must call basedir before install")))
        };
        me.installs.push((basedir, url));
        Ok(())
    }
    fn delete_unmatched(&self, _lua: &Lua, target: String) -> mlua::Result<()> {
        if target.ends_with("/") {
            return Err(mlua::Error::RuntimeError(format!("A glob ending in \"/\" is not allowed here.")));
        }
        let glob = match Glob::new(&target) {
            Ok(x) => x,
            Err(x) => {
                return Err(mlua::Error::RuntimeError(format!("Invalid glob {:?}: {}", target, x))); 
            },
        };
        if glob.has_root() || glob.has_semantic_literals() {
            return Err(mlua::Error::RuntimeError(format!("Rooted globs, and semantic components (such as \"..\"), are not allowed")));
        }
        let mut me = self.refmut()?;
        let basedir = if let Some(basedir) = me.basedir.as_ref() { basedir.clone() }
        else {
            return Err(mlua::Error::RuntimeError(format!("You must call basedir before install")))
        };
        match me.deletes.entry(basedir) {
            HashMapEntry::Occupied(mut ent) => { ent.get_mut().push(target); }
            HashMapEntry::Vacant(ent) => { ent.insert(vec![target]); }
        }
        Ok(())
    }
}

pub fn find_updates(gui: Rc<RefCell<dyn Gui>>, verbose: bool, body: &[u8], url: Url) -> Result<(Vec<(PathBuf, Url)>, HashMap<PathBuf, Vec<String>>), ()> {
    const UNSAFE_FUNCTIONS: &[&str] = &[
        "dofile", "loadfile",
    ];
    let lua = match mlua::Lua::new_with(mlua::StdLib::COROUTINE | mlua::StdLib::MATH | mlua::StdLib::STRING | mlua::StdLib::TABLE, mlua::LuaOptions::new().catch_rust_panics(false)) {
        Ok(x) => x,
        Err(x) => {
            gui.borrow_mut().do_error("Internal error", &format!("Unable to initialize Lua. The error was:\n{}", x));
            return Err(());
        },
    };
    for func in UNSAFE_FUNCTIONS.iter() {
        lua.globals().set(*func, Nil).unwrap();
    }
    if cfg!(windows) { lua.globals().set("windows", true).unwrap(); }
    if cfg!(unix) { lua.globals().set("unix", true).unwrap(); }
    if cfg!(target_os="macos") { lua.globals().set("macos", true).unwrap(); }
    lua.globals().set("target_os", cfg!(target_os)).unwrap();
    lua.globals().set("target_family", cfg!(target_family)).unwrap();
    let uf = Rc::new(RefCell::new(UpdateFinder::new(gui.clone(), verbose, url)));
    if verbose {
        let gui = gui.clone();
        lua.globals().set("print", lua.create_function_mut(move |lua, things: MultiValue| { gui.borrow_mut().verbose(&things.into_iter().map(|x| String::from_lua(x, lua)).collect::<Result<Vec<String>, _>>()?.join("\t")); Ok(()) }).unwrap()).unwrap();
    }
    else {
        lua.globals().set("print", lua.create_function_mut(move |_lua, _things: MultiValue| -> Result<_, _> { Ok(()) }).unwrap()).unwrap();
    }
    lua.globals().set("getenv", lua.create_function_mut(move |_lua, env: String| {
        Ok(std::env::var(&env).ok())
    }).unwrap()).unwrap();
    {
        let uf = uf.clone();
        lua.globals().set("detect_dir", lua.create_function_mut(move |lua, param: (String, String, Function, Table)| {
            uf.detect_dir(lua, param.0, param.1, param.2, param.3)
        }).unwrap()).unwrap();
    }
    {
        let uf = uf.clone();
        lua.globals().set("basedir", lua.create_function_mut(move |lua, param: String| {
            uf.basedir(lua, param)
        }).unwrap()).unwrap();
    }
    {
        let uf = uf.clone();
        lua.globals().set("cd", lua.create_function_mut(move |lua, param: String| {
            uf.cd(lua, param)
        }).unwrap()).unwrap();
    }
    {
        let uf = uf.clone();
        lua.globals().set("sense", lua.create_function_mut(move |lua, param: String| {
            uf.sense(lua, param)
        }).unwrap()).unwrap();
    }
    {
        let uf = uf.clone();
        lua.globals().set("install", lua.create_function_mut(move |lua, param: String| {
            uf.install(lua, param)
        }).unwrap()).unwrap();
    }
    {
        let uf = uf.clone();
        lua.globals().set("delete_unmatched", lua.create_function_mut(move |lua, param: String| {
            uf.delete_unmatched(lua, param)
        }).unwrap()).unwrap();
    }
    {
        let gui = gui.clone();
        lua.globals().set("do_message", lua.create_function_mut(move |_lua, param: (String, String)| {
            gui.borrow_mut().do_message(&param.0, &param.1);
            Ok(())
        }).unwrap()).unwrap();
    }
    {
        let gui = gui.clone();
        lua.globals().set("do_warning", lua.create_function_mut(move |_lua, param: (String, String, Option<bool>)| {
            Ok(gui.borrow_mut().do_warning(&param.0, &param.1, param.2.unwrap_or(false)))
        }).unwrap()).unwrap();
    }
    {
        let gui = gui.clone();
        lua.globals().set("do_error", lua.create_function_mut(move |_lua, param: (String, String)| {
            gui.borrow_mut().do_error(&param.0, &param.1);
            Ok(())
        }).unwrap()).unwrap();
    }
    {
        lua.globals().set("bail_out", lua.create_function_mut(move |_lua, _: ()| -> mlua::Result<()> {
            Err(mlua::Error::ExternalError(Arc::new(BailOut)))
        }).unwrap()).unwrap();
    }
    match lua.load(body).set_name("@index").unwrap().exec() {
        Ok(_) => (),
        Err(x) => {
            if let mlua::Error::CallbackError { cause, .. } = x {
                let f = format!("{}", cause);
                if f != "BAIL OUT" {
                    gui.borrow_mut().do_error("Lua error", &format!("An error occurred while processing the update index. The error was:\n{}", cause));
                }
            }
            else {
                gui.borrow_mut().do_error("Lua error", &format!("An error occurred while processing the update index. The error was:\n{}", x));
            }
            return Err(());
        },
    }
    drop(lua);
    let uf = match Rc::try_unwrap(uf) {
        Ok(x) => x.into_inner(),
        Err(_) => panic!("Dangling reference to UpdateFinder"),
    };
    if verbose {
        gui.borrow_mut().verbose("Finished examining update index.");
    }
    Ok((uf.installs, uf.deletes))
}

#[derive(Debug)]
struct BailOut;
impl std::error::Error for BailOut {}

impl std::fmt::Display for BailOut {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "BAIL OUT")
    }
}

