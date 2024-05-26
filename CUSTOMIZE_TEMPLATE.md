# How to Build You Customize Template

customize template path:

    .zen/templates/starter

## Starter Init Templage 

Contansts:
`__app__`     : application name
`__group__`   : application maven group name
`__package__` : application base package name 

Configure your `ddd` architecture style template:

Replace your Project use above `contansts` ,and create  template directory `ddd_init` , the template name `ddd_init` can load by Zen .


`ddd_init` template directory structure list:


    ➜  __app__ git:(main) ✗ tree -L 1
    ├── .gitignore
    ├── .java-version
    ├── .zen
    ├── README.md
    ├── __app__-api
    ├── __app__-application
    ├── __app__-bootstrap
    ├── __app__-common
    ├── __app__-dependencies
    ├── __app__-domain
    ├── __app__-infrastructure
    ├── __app__-rpc
    ├── __app__-web
    └── pom.xml

.java_version is jenv version file ,default jdk version
.zen is Zen Utils default config ,set project metadata 

