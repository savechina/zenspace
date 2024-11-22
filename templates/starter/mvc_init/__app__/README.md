# BlueKit平台服务

## 介绍
 应用名称 ：__app__
## 系统概览

## 名词解释

```plain text
 airp       平台名称
 qms        系统名称
 center     应用名称
 rpc        运程过程调用：调用外部
 rule       规则
 bill       单据
 settle     结算
 detail     明细详情
```

## 开发规约


* 系统名称规则：
    ```bash
        01xxx-02xxxx-03xxxx
    ```

    * 01： 平台名称 : rep   
    * 02： 系统名称 : rebate   
    * 03： 应用名称 : center | man |oper | shop | soa | worker | fea


* 模块名称规则：
    ```bash
        01xxx-02xxxx-03xxxx-04xxx-05xxxx-06xx-07xx
    ```
    * 01： 平台名称 : rep   
    * 02： 系统名称 : rebate   
    * 03： 应用名称 : center
    * 04： 模块名称 : service; 
        * dependencies : 应用统一的依赖管理
        * api:应用对外服务接口定义(provider),主要是JSF 等服务框架
        * domain: 领域服务，领域模型，主要负责业务模型及业务逻辑处理
        * context: 领域活动内容，面向用例场景的领域角色和服务对象调度及事务处理
        * common: 公共工具类
        * application : 业务应用层，负责程序调度，进程状态流程处理
        * rpc : 调用外部服务包装
        * web : web 应用层的 REST 服务,
        * bootstrap : application bootstrap 服务 。profies 配置
        * infrastructure: 基础设施（repository,cache,mq）
        * facade: 基础设施防腐层，注意本模块Spring Bean 需要通过XML方式配置
        * sdk: 领域服务扩展SPI 接口
        * 
    * 05：扩展标识: extension;| extra;
    * 06：业务域： rule;
    * 07：站点区域：cn :中国; id : 印尼; th : 泰国
* 模块依赖关系：
* 
``` 
                        __app__-bootstrap +---+-----------------------+
                                    |                |                       |
                                    v                |                       | 
                        __app__-web           |                       |
                                    |                |                       |
                                    |                v                       V
                                    |  __app__-facade-cn   __app__-facade-id 
                                    |           |                        |
                                    v           v                        |
                        __app__-infrastructure  <-----------------+
                                    |
                                    v
                        __app__-application
                                    |
                                    v
                        __app__-context
                                    |
                                    v
                        __app__-domain
                             |      |        |
                             v      |        v           
        __app__-common    |      __app__-api
                                    |
                                    v
                            __app__-sdk
                                    |
                                    v
                        __app__-dependencies 

```

```
                            +---------------+               
                            |   bootstrap   +------+             
                            +------+--------+      |         
                                   |               |         
                                   |               |         
                                   v               |         
                            +--------------+       |         
                      +-----+infrastructure|       | 
                      |     +------+-------+       |        
                      |            |               |        
                      v            |               v        
               +--------------+    |        +--------------+
               | application  |    |        |     web      |
               +-----+--+-----+    |        +------+-------+
                     |  |          |               |        
                     |  |          |               |        
                     |  |          |               |        
                     |  |          v               |        
+--------------+     |  |   +--------------+       |        
|     api      |<----+  +-->|    domain    |<------+        
+--------------+            +---+-+-+------+                
                                | | |                       
                                | | |                       
                                | | |                       
          +--------------+      | | |       +--------------+
          |     sdk      |<-----+ | +------>|     rpc      |
          +--------------+        |         +--------------+
                                  |                         
                                  v                         
                           +--------------+                 
                           |    common    |                 
                           +--------------+                 
```

* 类命名规约：
    * xxxMO: API | RPC | MQ | SPI 等接口Bean 参数或者返回结果
    * xxxDTO: service 外部服务传输对象, 等同于 xxxDTO，
    * xxxDO: 领域服务内部，领域对象 ,业务对象；
    * xxxBO:  service内部，业务对象Bean ; 等同于 xxxDO
    * xxxEntity: 数据库DB存储实体对象;
    * xxxVO: Web Controller 页面显示Bean
    * xxxRPC: RPC 调用外部服务接口定义
    * xxxMessageConsumer: MQ 消息消费者处理器
    * xxxMessageProducer: MQ 消息发送者处理器
    * xxxServiceProvider: API 对外服务接口定义 ;
    * xxxServiceExtension: SDK或 SPI 内部扩展服务接口定义 ;
    * xxxImpl:  接口实现类定义后缀;
    * Airpxxx: 类名前辍定义
    * xxxAbility: 领域能力后缀
    * xxxAbilityExt: 能力扩展点接口后缀
    * xxxDomainService：
    * xxxDomainActivity：
    * xxxRepository： 数据持久访问操作接口定义；包括database，ES等数据存储容器；
    * xxxEntityConverter： xxx实体对象转换器；
   
 

* Center 包结构规约：

    package __package__.*
    * common: 公共用途
      * constants:  公共常量
      * exception: 公共Exception异常
      * enums: 枚举
      * utils: 工具类
    * core: 核心层 包，｜ 此包层是可选的
        * domain:
             * rule(xxx)  #领域包 返利规则
                * model
                * repository 资源库接口
                * ability    领域能力
                * extension  领域能力扩展默认实现
                * service    领域服务
                    * impl
             * bill  账单   
             * settle  结算
             * attachment 附件
        * product 水平业务领域产品
            * ruleA:  A 领域产品
        * rpc 三方依赖接口定义
            * vendor: 供应商资源接口
            * organization: 组织机构接口
    * domain: 领域服务层包
      * cache:
      * config:
      * model:
      * repository:
      * service: 领域服务定义
        * xxx(sku) : xxx领域服务
      * vo: 视图对象定义
    * context: 场景上下文 ｜ 此包层是可选的
        * activity:  业务action融合
        * specification:  
    * application: 应用层
        * task: 调度worker
        * message:   MQ 消息接口. alias: mq;
        * provider:  对外服务接口API 实现
    * rpc: 依赖三方服务接口定义
    * infrastructure: 基础设施
        * repository:   资源库实现
        * entity:       Entity 对象
        * convert:      Entity Converter
        * rpc:          三方资源接口定义实现
        * message:      MQ 消息接口实现
    * facade ：  防腐层  ｜ 此包层是可选的
        * id:
        * cn:
            * message : 消息个性接口防腐实现
            * rpc:    三方资源个性接口防腐实现
            * dao:  数据访问个性接口防腐实现
    * api: 对外提供API 接口
        * domain:   开放服务API. xxxServiceProvider ,xxxServiceReadProvider, xxxServiceWriteProvider 
    * sdk: 可扩展服务接口SPI定义  ｜ 此包层是可选的
        * domain:    领域业务能力
            * rule:
                * mo:   能力扩展点领域Model
                * spi:  能力扩展点SPI接口定义
    * web:
      * controller: web rest 服务
      * interceptor: 拦截器

* App 包结构规约：

    package __package__
    * common: 公共用途
    * context:  
    * infrastructure: 基础设施
        * repository:   资源库实现
    * application:
        * domain:    领域业务能力
            * rule:
                * mo:   能力扩展点领域Model
                * spi:  能力扩展点SPI接口定义
        * product:    领域产品扩展点
            * ruleA:   A 领域产品
                * mo:
                * spi:

* 开发注意事项FAQ                    
    1. 项目启动环境配置设置
        * profiles选择 artifactory 和 develop
        * tomcat版本用tomcat9
    2. Facade 模块Bean 配置方式
        * 涉及模块： __app__-facade-cn 和 __app__-facade-id
        * 模块Class Bean初始化注入方式，不要使用 @Service 等注释方式，请使用XML配置文件方式
        * Maven 打包时通过Profile 配置不同的 Site(cn,id) ，加载注入配置文件，动态进行Bean 实现注入；
        
        
* 开发注意变动点
    

|开发变动点|PaaS开发地方|备注|
|---|---|---|
|API 接口定义|没有变化，PaaS API 对外提供接口定义还在 api 模块||
|新增Maven 模块|新增context、SDK、product、facade||
|dao变动|需要把dao 接口迁移到基础设施层，包括entity||

* Spring Boot 启动JVM 配置：
`  --add-opens java.base/java.lang=ALL-UNNAMED --add-opens java.base/java.lang.reflect=ALL-UNNAMED --add-opens java.base/java.util=ALL-UNNAMED --add-opens java.base/java.base=ALL-UNNAMED --add-opens java.base/java.io=ALL-UNNAMED --add-opens java.base/sun.util=ALL-UNNAMED --add-opens java.base/sun.util.calendar=ALL-UNNAMED --add-opens java.base/java.math=ALL-UNNAMED --add-opens java.base/sun.security.action=ALL-UNNAMED --add-exports=java.base/sun.net.util=ALL-UNNAMED " `